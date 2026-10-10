//! ProcLens collector. Subcommands:
//!   serve [--idle-exit SECS]   the long-running elevated service the UI connects to
//!   mem                        memory breakdown (page lists need elevation)
//!   procs                      process list baseline
//!   etw [seconds]              kernel trace spike (run elevated)

mod activity;
mod etw;
mod files;
mod mem;
mod pipe;
mod procs;
mod profile;
mod serve;
mod trace;

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => {
            // Default: exit 120 s after the last client leaves. 0 disables.
            let mut idle = Some(Duration::from_secs(120));
            if let Some(i) = args.iter().position(|a| a == "--idle-exit") {
                match args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    Some(0) => idle = None,
                    Some(n) => idle = Some(Duration::from_secs(n)),
                    None => {
                        eprintln!("--idle-exit needs a number of seconds");
                        std::process::exit(2);
                    }
                }
            }
            std::process::exit(serve::run(idle));
        }
        Some("profile") => profile::run(),
        Some("mem") => mem::run(),
        Some("procs") => procs::run(),
        Some("etw") => {
            let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
            etw::run(secs);
        }
        _ => eprintln!("usage: collector <serve [--idle-exit SECS] | mem | procs | etw [seconds]>"),
    }
}
