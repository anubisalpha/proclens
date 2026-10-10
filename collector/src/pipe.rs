//! Server end of the collector pipe, plus the token helpers it needs.
//!
//! Hardening, each with the attack it addresses:
//! - DACL grants access only to the current user's SID: other local users can't connect.
//! - `FILE_FLAG_FIRST_PIPE_INSTANCE` on the first instance: if another process already owns the
//!   name, creation fails instead of silently sharing it (pipe squatting).
//! - `PIPE_REJECT_REMOTE_CLIENTS`: no connections over SMB from other machines.
//! - Mandatory label "no-write-up, Medium": the normal-privilege UI may connect, but Low-integrity
//!   (sandboxed) processes of the same user may not.

use std::fs::File;
use std::io;
use std::os::windows::io::FromRawHandle;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
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

/// The current user's SID as a string like `S-1-5-21-...`.
pub fn user_sid_string() -> io::Result<String> {
    with_token(|token| unsafe {
        let mut ret = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut ret); // asks for the size
        let mut buf = vec![0u8; ret as usize];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut _), ret, &mut ret)
            .map_err(io::Error::other)?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).map_err(io::Error::other)?;
        let out = s.to_string().map_err(io::Error::other);
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut _)));
        out
    })
}

pub struct PipeServer {
    name: Vec<u16>,
    sa: SECURITY_ATTRIBUTES,
    first: bool,
}

// The security descriptor is allocated once and never freed or mutated; sharing it is safe.
unsafe impl Send for PipeServer {}

impl PipeServer {
    pub fn new(pipe_name: &str) -> io::Result<Self> {
        let sid = user_sid_string()?;
        // D:P = protected DACL, one ACE: GenericAll for this user only. S: = integrity label.
        let sddl = format!("D:P(A;;GA;;;{sid})S:(ML;;NW;;;ME)");
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
