//! Server end of the collector pipe, plus the token helpers it needs.
//!
//! Hardening, each with the attack it addresses:
//! - DACL grants access only to the current user's SID: other local users can't connect.
//! - `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance: if another process already owns the
//!   name, creation fails instead of silently sharing it (pipe squatting).
//! - `PIPE_REJECT_REMOTE_CLIENTS`: no connections over SMB from other machines.
//! - Mandatory label "no-write-up, Medium": the normal-privilege UI may connect, but Low-integrity
//!   (sandboxed) processes of the same user may not.
//!
//! What this does *not* do: tell the UI apart from other Medium-integrity processes of the same
//! user. Any such process can connect, read what the elevated collector reports (per-process I/O
//! and file paths of processes it could not inspect itself) and send `Shutdown`. A server-side
//! check of the client's image path would not close that (same-user code can inject into the UI,
//! and the example tools connect from elsewhere), so the boundary is "the logged-on user", the
//! same one Task Manager's elevated view draws. See "Security notes" in the README.

use std::fs::File;
use std::io;
use std::os::windows::io::FromRawHandle;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION,
    TOKEN_QUERY,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn with_token<T>(f: impl FnOnce(HANDLE) -> io::Result<T>) -> io::Result<T> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(io::Error::other)?;
        let r = f(token);
        let _ = CloseHandle(token);
        r
    }
}

pub fn is_elevated() -> bool {
    with_token(|token| unsafe {
        let mut e = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut e as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .map_err(io::Error::other)?;
        Ok(e.TokenIsElevated != 0)
    })
    .unwrap_or(false)
}

/// True if `s` looks like a SID string (`S-1-5-21-...`). The value goes into an SDDL string, so
/// anything that isn't plain digits and dashes after `S-1-` is refused rather than escaped.
pub fn is_valid_sid_string(s: &str) -> bool {
    s.len() <= 184
        && s.strip_prefix("S-1-").is_some_and(|rest| {
            !rest.is_empty()
                && !rest.starts_with('-')
                && !rest.ends_with('-')
                && !rest.contains("--")
                && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        })
}

/// The security descriptor for the pipe: a protected DACL with one GenericAll ACE per distinct
/// SID, and a Medium no-write-up integrity label.
fn build_sddl(sids: &[String]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    let mut aces = String::new();
    for sid in sids {
        if !seen.contains(&sid.as_str()) {
            seen.push(sid);
            aces.push_str(&format!("(A;;GA;;;{sid})"));
        }
    }
    format!("D:P{aces}S:(ML;;NW;;;ME)")
}

pub struct PipeServer {
    name: Vec<u16>,
    sa: SECURITY_ATTRIBUTES,
    first: bool,
}

// The security descriptor is allocated once and never freed or mutated; sharing it is safe.
unsafe impl Send for PipeServer {}

impl PipeServer {
    /// `also_allow` lists further SIDs that may connect besides this process's own user: the UI's
    /// account, which differs when a standard user approved the UAC prompt with an administrator's
    /// credentials (then this process runs as that administrator).
    pub fn new(pipe_name: &str, also_allow: &[String]) -> io::Result<Self> {
        let mut sids = vec![proclens_proto::client::current_user_sid()?];
        for sid in also_allow {
            if !is_valid_sid_string(sid) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a SID: {sid:?}")));
            }
            sids.push(sid.clone());
        }
        // D:P = protected DACL, GenericAll per listed SID. S: = integrity label.
        let sddl = build_sddl(&sids);
        let sddl_w: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut psd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl_w.as_ptr()),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
        }
        .map_err(io::Error::other)?;
        Ok(Self {
            name: pipe_name.encode_utf16().chain([0]).collect(),
            sa: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd.0,
                bInheritHandle: false.into(),
            },
            first: true,
        })
    }

    /// Creates the next pipe instance and blocks until a client connects to it.
    /// The very first call fails if the pipe name is already taken.
    pub fn accept(&mut self) -> io::Result<File> {
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if self.first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(self.name.as_ptr()),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                Some(&self.sa),
            )
        };
        if handle.is_invalid() {
            return Err(io::Error::last_os_error());
        }
        self.first = false;
        // Takes ownership: the File closes the handle even if connecting fails below.
        let file = unsafe { File::from_raw_handle(handle.0) };
        if let Err(e) = unsafe { ConnectNamedPipe(handle, None) } {
            // The client may connect between CreateNamedPipe and ConnectNamedPipe.
            if unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                return Err(io::Error::other(e));
            }
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_strings_are_validated_before_they_reach_sddl() {
        assert!(is_valid_sid_string("S-1-5-21-1004336348-1177238915-682003330-1001"));
        assert!(is_valid_sid_string("S-1-5-18"));
        for bad in ["", "S-1-", "S-1--5", "S-1-5-", "S-1-5-21)(A;;GA;;;WD", "s-1-5-18", "S-1-5-1 8", "S-2-5-18", "S-1-5--18", "S-1-5-18
"] {
            assert!(!is_valid_sid_string(bad), "{bad:?} must be refused");
        }
        assert!(!is_valid_sid_string(&format!("S-1-{}", "1".repeat(200))));
    }

    #[test]
    fn sddl_has_one_ace_per_distinct_sid_and_the_label() {
        let sddl = build_sddl(&["S-1-5-21-1".to_string(), "S-1-5-21-2".to_string(), "S-1-5-21-1".to_string()]);
        assert_eq!(sddl, "D:P(A;;GA;;;S-1-5-21-1)(A;;GA;;;S-1-5-21-2)S:(ML;;NW;;;ME)");
    }
}
