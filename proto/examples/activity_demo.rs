//! Streaming test: `activity_demo <collector.exe>`.
//! Subscribes to Activity, generates a known file workload in THIS process (64 MiB written and
//! read back) and checks that the snapshots account for it. Also prints the profile's self-tests.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::exit;
use std::time::{Duration, Instant};

use proclens_proto::{client, ActivitySnapshot, Request, Response, Topic};

const MIB: u64 = 1 << 20;
const WORKLOAD_MIB: u64 = 64;

fn main() {
    let exe = std::env::args().nth(1).map(PathBuf::from).expect("usage: activity_demo <collector.exe>");
    let exe = std::fs::canonicalize(exe).expect("collector.exe not found");

    let mut conn = match client::connect(&exe, "activity_demo") {
        Ok(c) => c,
        Err(e) if e.raw_os_error() == Some(2) => {
            println!("launching collector elevated (approve the UAC prompt)...");
            client::launch_elevated(&exe, &launch_args()).expect("launch");
            client::connect_with_retry(&exe, "activity_demo", Duration::from_secs(45)).expect("connect")
        }
        Err(e) => {
            println!("REFUSED: {e}");
            exit(3);
        }
    };
    println!("connected: {:?}", conn.ack);

    conn.send(&Request::Snapshot { topic: Topic::Profile }).unwrap();
    conn.send(&Request::Subscribe { topic: Topic::Activity, interval_ms: 2000 }).unwrap();

    let me = std::process::id();
    let name = format!("activity-demo-{me}.bin");
    let path = std::env::temp_dir().join(&name);
    let mut workload_started = false;
    let mut snapshots: Vec<ActivitySnapshot> = Vec::new();
    let t0 = Instant::now();

    while snapshots.len() < 4 && t0.elapsed() < Duration::from_secs(40) {
        match conn.poll(Duration::from_millis(100)).unwrap() {
            client::Poll::Message(Response::Profile(p)) => {
                println!("profile: {:?}, build {}.{}", p.verification, p.os.build, p.os.ubr);
                for c in &p.capabilities {
                    println!("  {:<20} {:?}  {}", c.id, c.status, c.detail);
                }
            }
            client::Poll::Message(Response::Subscribed { topic, interval_ms }) => {
                println!("subscribed {topic:?} every {interval_ms} ms; starting workload");
                workload_started = true;
            }
            client::Poll::Message(Response::Activity(a)) => {
                println!("snapshot {}: interval {} ms, {} processes, {} files, {} drivers", snapshots.len() + 1, a.interval_ms, a.processes.len(), a.files.len(), a.drivers.len());
                snapshots.push(a);
            }
            client::Poll::Message(Response::Error { message }) => {
                println!("collector error: {message}");
                exit(5);
            }
            client::Poll::Message(_) | client::Poll::Nothing => {}
            client::Poll::Closed => {
                println!("collector closed the connection");
                exit(4);
            }
        }
        // Run the workload once, after the trace is up and in the middle of the snapshots.
        if workload_started && snapshots.len() == 1 && !path.exists() {
            let chunk = vec![0x5Au8; MIB as usize];
            let mut f = std::fs::File::create(&path).unwrap();
            for _ in 0..WORKLOAD_MIB {
                f.write_all(&chunk).unwrap();
            }
            f.sync_all().unwrap();
            drop(f);
            let mut f = std::fs::File::open(&path).unwrap();
            let mut buf = vec![0u8; MIB as usize];
            while f.read(&mut buf).unwrap() > 0 {}
            println!("workload done: wrote and read {WORKLOAD_MIB} MiB in {name}");
        }
    }
    let _ = std::fs::remove_file(&path);

    // Add up what the snapshots say about this process and the file.
    let (mut my_read, mut my_write, mut file_total) = (0u64, 0u64, 0u64);
    for s in &snapshots {
        if let Some(p) = s.processes.iter().find(|p| p.pid == me) {
            my_read += p.file_read_bytes;
            my_write += p.file_write_bytes;
        }
        file_total += s.files.iter().filter(|f| f.path.ends_with(&name)).map(|f| f.bytes).sum::<u64>();
    }
    let want = WORKLOAD_MIB * MIB;
    println!("\nthis process: file reads {:.1} MiB, writes {:.1} MiB (expected >= {} each)", my_read as f64 / MIB as f64, my_write as f64 / MIB as f64, WORKLOAD_MIB);
    println!("the workload file: {:.1} MiB (expected ~{} = write + read)", file_total as f64 / MIB as f64, 2 * WORKLOAD_MIB);
    let last = snapshots.last().expect("no snapshots");
    println!("last snapshot health: {:?}", last.health);
    println!("top drivers by DPC+ISR time:");
    for d in last.drivers.iter().take(3) {
        println!("  {:<22} DPC {:>6} calls {:>9.0} us (max {:>6.0}), ISR {:>6} calls {:>9.0} us", d.name, d.dpc_calls, d.dpc_time_us, d.dpc_max_us, d.isr_calls, d.isr_time_us);
    }
    // Writes must be ~64 MiB, not ~128: cache-flush (paging) traffic must not be double counted.
    let ok = my_read >= want && my_write >= want && my_write <= want + want / 4 && file_total >= 2 * want && file_total <= 3 * want;
    println!("{}", if ok { "WORKLOAD ACCOUNTED FOR: OK" } else { "WORKLOAD MISMATCH" });

    conn.send(&Request::Unsubscribe { topic: Topic::Activity }).unwrap();
    conn.send(&Request::Shutdown).unwrap();
    loop {
        match conn.poll(Duration::from_secs(3)).unwrap() {
            client::Poll::Message(Response::Bye) | client::Poll::Closed | client::Poll::Nothing => break,
            _ => {}
        }
    }
    exit(if ok { 0 } else { 1 });
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
