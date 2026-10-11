//! ProcLens collector. Subcommands:
//!   serve [--idle-exit SECS] [--allow-sid SID]
//!                              the long-running elevated service the UI connects to
//!   profile                    this machine's OS info and self-test results
//!   discover [--emit FILE]     derive this build's ETW tables and compare with the profile
//!   mem                        memory breakdown (page lists need elevation)
//!   procs                      process list baseline
//!   etw [seconds]              kernel trace spike (run elevated)

mod activity;
mod discover;
mod etw;
mod files;
mod mem;
mod params;
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
            // The UI passes its own SID: when a standard user approves UAC with an administrator's
            // credentials, this process runs as that administrator and would otherwise lock the UI out.
            let mut allow = Vec::new();
            for (i, a) in args.iter().enumerate() {
                if a == "--allow-sid" {
                    match args.get(i + 1) {
                        Some(sid) if pipe::is_valid_sid_string(sid) => allow.push(sid.clone()),
                        _ => {
                            eprintln!("--allow-sid needs a SID like S-1-5-21-...");
                            std::process::exit(2);
                        }
                    }
                }
            }
            std::process::exit(serve::run(idle, allow));
        }
        Some("profile") => profile::run(),
        Some("discover") => {
            // discover [--emit FILE]: derive this build's ETW tables from live events and compare
            // them with the profile (needs an elevated process).
            let emit = args.iter().position(|a| a == "--emit").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
            discover::run(emit);
        }
        Some("mem") => mem::run(),
        Some("procs") => procs::run(),
        Some("etw") => {
            let secs = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
            etw::run(secs);
        }
        _ => eprintln!(
            "usage: collector <serve [--idle-exit SECS] [--allow-sid SID] | profile | discover [--emit FILE] | mem | procs | etw [seconds]>"
        ),
    }
}
