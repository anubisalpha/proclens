//! Negative test: an unprivileged process that grabs the collector's pipe name first.
//! `squat [seconds]` holds the name with a default ACL. Both the real collector (must refuse to
//! start) and the client (must refuse to talk to it) should detect this.

use std::time::Duration;

use proclens_proto::PIPE_NAME;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(30);
    let name: Vec<u16> = PIPE_NAME.encode_utf16().chain([0]).collect();
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            255,
            4096,
            4096,
            0,
            None,
        )
    };
    if h.is_invalid() {
        println!("squatter could not create the pipe: {}", std::io::Error::last_os_error());
        return;
    }
    println!("squatting on {PIPE_NAME} for {secs}s (pid {})", std::process::id());
    // Accept one connection and then say nothing, like a hostile server that just wants to be
    // talked to. (Blocks until a client arrives; the process is killed by the test harness
    // after `secs`, so a client that never comes still ends the test.)
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        println!("squatter done");
        std::process::exit(0);
    });
    let _ = unsafe { ConnectNamedPipe(h, None) };
    println!("a client connected to the squatter");
    std::thread::sleep(Duration::from_secs(secs));
}
