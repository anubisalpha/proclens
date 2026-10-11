//! Unelevated client smoke test: `client_demo <path to collector.exe>`.
//! Launches the collector elevated if it isn't running (one UAC prompt), then exercises every
//! request type, including a malformed one, and finally shuts the collector down.

use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use proclens_proto::{client, Request, Response, Topic};

fn main() {
    let exe = std::env::args().nth(1).map(PathBuf::from).expect("usage: client_demo <collector.exe>");
    let exe = std::fs::canonicalize(exe).expect("collector.exe not found");

    let mut conn = match client::connect(&exe, "client_demo") {
        Ok(c) => c,
        Err(e) if e.raw_os_error() == Some(2) => {
            println!("collector not running; launching it elevated (approve the UAC prompt)...");
            if let Err(e) = client::launch_elevated(&exe, &launch_args()) {
                println!("LAUNCH FAILED: {e}");
                exit(2);
            }
            match client::connect_with_retry(&exe, "client_demo", Duration::from_secs(30)) {
                Ok(c) => c,
                Err(e) => {
                    println!("REFUSED/FAILED after launch: {e}");
                    exit(3);
                }
            }
        }
        Err(e) => {
            println!("REFUSED: {e}");
            exit(3);
        }
    };
    println!("connected to collector pid {}: {:?}", conn.server_pid, conn.ack);

    conn.send(&Request::Subscribe { topic: Topic::Memory, interval_ms: 500 }).unwrap();
    conn.send(&Request::Subscribe { topic: Topic::Processes, interval_ms: 1000 }).unwrap();
    // Not a valid request: the collector must answer with Error and keep the connection.
    conn.send(&serde_json::json!({"type": "kill_process", "pid": 4})).unwrap();
    // Out-of-range interval must be clamped, not obeyed.
    conn.send(&Request::Subscribe { topic: Topic::Memory, interval_ms: 1 }).unwrap();

    let (mut memory, mut processes, mut errors) = (0, 0, 0);
    while memory < 4 || processes < 3 || errors < 1 {
        match conn.recv() {
            Ok(Some(Response::Memory(m))) => {
                memory += 1;
                if memory == 1 {
                    let gb = |b: u64| b as f64 / 1073741824.0;
                    println!(
                        "memory: total {:.1} GB, available {:.1} GB, page lists {}",
                        gb(m.physical_total),
                        gb(m.physical_available),
                        match &m.lists {
                            Some(l) => format!("PRESENT (in use {:.1} GB, standby {:.1} GB)", gb(l.in_use), gb(l.standby)),
                            None => "absent".to_string(),
                        }
                    );
                }
            }
            Ok(Some(Response::Processes(p))) => {
                processes += 1;
                if processes == 1 {
                    println!("processes: {} listed, {:.1} GB working set", p.processes.len(), p.total_working_set as f64 / 1073741824.0);
                }
            }
            Ok(Some(Response::Subscribed { topic, interval_ms })) => {
                println!("subscribed {topic:?} at {interval_ms} ms")
            }
            Ok(Some(Response::Error { message })) => {
                errors += 1;
                println!("collector error (expected for the malformed request): {message}");
            }
            Ok(Some(other)) => println!("other: {other:?}"),
            Ok(None) => {
                println!("collector closed the connection unexpectedly");
                exit(4);
            }
            Err(e) => {
                println!("read error: {e}");
                exit(4);
            }
        }
    }
    println!("received {memory} memory, {processes} process messages, {errors} error");

    conn.send(&Request::Shutdown).unwrap();
    loop {
        match conn.recv() {
            Ok(Some(Response::Bye)) => {
                println!("collector said Bye");
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => {
                println!("connection closed");
                break;
            }
        }
    }
    println!("OK");
}

/// The arguments the UI itself uses to launch the collector (its own SID goes along, so a collector
/// elevated under another account still lets this process in).
fn launch_args() -> String {
    let mut args = String::from("serve --idle-exit 20");
    if let Ok(sid) = client::current_user_sid() {
        args.push_str(" --allow-sid ");
        args.push_str(&sid);
    }
    args
}
