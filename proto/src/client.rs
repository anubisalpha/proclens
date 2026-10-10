//! Client side of the collector pipe, for the unelevated UI.
//!
//! Threat being handled: the UI runs at normal privilege, so any other process of the same user
//! could create the pipe name first and impersonate the collector (feeding the UI false data, or
//! phishing it). The collector prevents the reverse by creating the pipe with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE`. The client therefore checks who owns the server end *before*
//! sending anything, by comparing that process's image path with the expected collector binary.
//!
//! That check is only as strong as the binary's location: a release build must install the
//! collector somewhere a normal user cannot write (e.g. Program Files). A dev build in a
//! user-writable folder gets the check but not that guarantee.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, PeekNamedPipe};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::frame::{read_frame, write_frame};
use crate::{Request, Response, PIPE_NAME, PROTOCOL_VERSION};

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;
const ERROR_CANCELLED: i32 = 1223;

pub struct Connection {
    file: File,
    pub server_pid: u32,
    /// The collector's HelloAck.
    pub ack: Response,
}

impl Connection {
    /// Generic so tests can send deliberately malformed requests; normal callers pass [`Request`].
    pub fn send<T: serde::Serialize>(&mut self, req: &T) -> io::Result<()> {
        write_frame(&mut self.file, req)
    }

    /// Blocks until a message arrives. `Ok(None)` means the collector closed the connection.
    /// Don't call this from a thread that also needs to [`send`](Self::send): a blocked read on a
    /// synchronous pipe handle stalls writes. Use [`poll`](Self::poll) from a single I/O thread.
    pub fn recv(&mut self) -> io::Result<Option<Response>> {
        read_frame(&mut self.file)
    }

    /// Waits up to `timeout` for a message without blocking writes for longer than that.
    pub fn poll(&mut self, timeout: Duration) -> io::Result<Poll> {
        let deadline = Instant::now() + timeout;
        loop {
            let mut avail = 0u32;
            let peek = unsafe {
                PeekNamedPipe(HANDLE(self.file.as_raw_handle()), None, 0, None, Some(&mut avail), None)
            };
            if peek.is_err() {
                return Ok(Poll::Closed); // broken pipe: the collector went away
            }
            if avail > 0 {
                return Ok(match read_frame(&mut self.file)? {
                    Some(r) => Poll::Message(r),
                    None => Poll::Closed,
                });
            }
            if Instant::now() >= deadline {
                return Ok(Poll::Nothing);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[derive(Debug)]
pub enum Poll {
    Message(Response),
    Nothing,
    Closed,
}

/// Connects once. Fails if the pipe does not exist, is owned by an unexpected process, or speaks
/// a different protocol version.
pub fn connect(expected_server_exe: &Path, client_name: &str) -> io::Result<Connection> {
    let file = OpenOptions::new().read(true).write(true).open(PIPE_NAME)?;

    let handle = HANDLE(file.as_raw_handle());
    let mut pid = 0u32;
    unsafe { GetNamedPipeServerProcessId(handle, &mut pid) }.map_err(io::Error::other)?;
    verify_server_image(pid, expected_server_exe)?;

    let mut conn = Connection {
        file,
        server_pid: pid,
        ack: Response::Bye, // replaced below
    };
    conn.send(&Request::Hello {
        protocol_version: PROTOCOL_VERSION,
        client: client_name.to_string(),
    })?;
    match conn.recv()? {
        Some(ack @ Response::HelloAck { protocol_version, collector_pid, .. }) => {
            if protocol_version != PROTOCOL_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("collector speaks protocol {protocol_version}, UI speaks {PROTOCOL_VERSION}"),
                ));
            }
            if collector_pid != pid {
                // The process serving the pipe is not the one that claims to be the collector.
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "collector pid mismatch"));
            }
            conn.ack = ack;
            Ok(conn)
        }
        Some(Response::Error { message }) => Err(io::Error::other(message)),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "expected HelloAck")),
    }
}

/// Like [`connect`], but waits for the pipe to appear (e.g. right after [`launch_elevated`]).
/// Only "not there yet" and "busy" are retried; a wrong owner or protocol is returned at once.
pub fn connect_with_retry(
    expected_server_exe: &Path,
    client_name: &str,
    timeout: Duration,
) -> io::Result<Connection> {
    let deadline = Instant::now() + timeout;
    loop {
        match connect(expected_server_exe, client_name) {
            Ok(c) => return Ok(c),
            Err(e) if matches!(e.raw_os_error(), Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY)) => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "collector did not appear"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

fn verify_server_image(pid: u32, expected: &Path) -> io::Result<()> {
    let actual = unsafe {
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, format!("OpenProcess({pid}): {e}")))?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(proc, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(proc);
        r.map_err(io::Error::other)?;
        String::from_utf16_lossy(&buf[..len as usize])
    };
    let norm = |p: &Path| -> io::Result<String> {
        Ok(std::fs::canonicalize(p)?.to_string_lossy().to_lowercase())
    };
    if norm(Path::new(&actual))? == norm(expected)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("pipe is served by pid {pid} ({actual}), not the expected collector"),
        ))
    }
}

/// Starts `exe` elevated (one UAC prompt). Returns once the process was launched, not once it is
/// ready: follow with [`connect_with_retry`]. A declined prompt is `ErrorKind::PermissionDenied`.
pub fn launch_elevated(exe: &Path, args: &str) -> io::Result<()> {
    let exe_w: Vec<u16> = exe.as_os_str().to_string_lossy().encode_utf16().chain([0]).collect();
    let args_w: Vec<u16> = args.encode_utf16().chain([0]).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(args_w.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| {
        // HRESULT_FROM_WIN32(ERROR_CANCELLED) when the user clicks "No" on the UAC prompt.
        if e.code().0 as u32 & 0xFFFF == ERROR_CANCELLED as u32 {
            io::Error::new(io::ErrorKind::PermissionDenied, "elevation was declined")
        } else {
            io::Error::other(e)
        }
    })
}
