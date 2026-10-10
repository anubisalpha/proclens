//! `collector etw [seconds]`: runs one trace session for a fixed time and prints a report.
//! This is the reference/diagnostic view of exactly the data `serve` streams to the UI, plus the
//! raw event table that shows what a new Windows build actually sends.

use std::collections::HashMap;
use std::time::Duration;

use crate::activity::{self, volume_map};
use crate::procs::Sampler;
use crate::trace::{driver_table, TraceSession};

fn mb(b: u64) -> f64 {
    b as f64 / 1048576.0
}

pub fn run(secs: u64) {
    println!("Starting kernel trace for {secs}s (needs admin)...");
    let session = match TraceSession::start("ProcLensSpike") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    std::thread::sleep(Duration::from_secs(secs));
    let health = session.health();
    let iv = session.take_interval();
    let diag = session.diagnostics();
    let freq = session.qpc_freq;
    session.stop();

    let drivers = driver_table();
    let names: HashMap<u32, String> =
        Sampler::new().processes().processes.into_iter().map(|p| (p.pid, p.name)).collect();
    let snap = activity::build(&iv, &names, health.as_ref(), freq, &drivers, &volume_map());

    println!("\n== Session health (ControlTrace QUERY) ==");
    match &health {
        Some(h) => println!(
            "events lost {}, buffers lost {}, realtime buffers lost {}, buffers written {}, buffers {} (free {})",
            h.events_lost, h.log_buffers_lost, h.rt_buffers_lost, h.buffers_written, h.buffers, h.free_buffers
        ),
        None => println!("query failed"),
    }

    println!("\n== Events seen (kind / opcode / name -> count) ==");
    for (kind, op, name, n) in &diag.seen {
        println!("{kind:<9} op{op:<3} {name:<22} {n}");
    }
    println!("schema lookup failures: {}", diag.schema_errors);

    println!("\n== Parse failures (field names that are wrong/missing on this build) ==");
    if diag.parse_errors.is_empty() {
        println!("none");
    }
    for (k, n) in &diag.parse_errors {
        println!("{k}  x{n}");
    }

    println!(
        "\n== Disk I/O by process (attributed via thread map: {}, fell back to header pid: {}) ==",
        iv.disk_attributed, iv.disk_fallback
    );
    println!("{:>7}  {:>9}  {:>9}  {:>7}  {:>10}  name", "pid", "read MB", "write MB", "ops", "latency us");
    let mut by_disk: Vec<_> = snap.processes.iter().filter(|p| p.disk_ops > 0).collect();
    by_disk.sort_by_key(|p| std::cmp::Reverse(p.disk_read_bytes + p.disk_write_bytes));
    for p in by_disk.iter().take(10) {
        println!(
            "{:>7}  {:>9.2}  {:>9.2}  {:>7}  {:>10}  {}",
            p.pid,
            mb(p.disk_read_bytes),
            mb(p.disk_write_bytes),
            p.disk_ops,
            p.disk_latency_us.map_or("-".to_string(), |l| format!("{l:.0}")),
            p.name
        );
    }

    println!("\n== File I/O accounting ==");
    println!(
        "requested {:.1} MB vs actually transferred {:.1} MB; failed ops {}; completions with no matching request {}",
        mb(iv.requested_total),
        mb(iv.actual_total),
        iv.io_failed,
        iv.io_unmatched_end
    );
    println!("paging I/O (cache flushes, page-ins; excluded from the figures below): {:.1} MB", mb(iv.paging_bytes));
    let total = snap.health.file_bytes_total;
    println!(
        "names from Create events {}, from handle table {}; unresolved bytes {:.1} of {:.1} MB ({:.1}%)",
        diag.names_from_create,
        diag.names_from_handles,
        mb(snap.health.unresolved_file_bytes),
        mb(total),
        if total > 0 { snap.health.unresolved_file_bytes as f64 * 100.0 / total as f64 } else { 0.0 }
    );

    println!("\n== File I/O by process (actual bytes; pid from the request) ==");
    let mut by_file: Vec<_> = snap.processes.iter().filter(|p| p.file_ops > 0).collect();
    by_file.sort_by_key(|p| std::cmp::Reverse(p.file_read_bytes + p.file_write_bytes));
    for p in by_file.iter().take(10) {
        println!(
            "{:>7}  {:>9.2}  {:>9.2}  {:>7}  {}",
            p.pid,
            mb(p.file_read_bytes),
            mb(p.file_write_bytes),
            p.file_ops,
            p.name
        );
    }

    println!("\n== Top files by bytes read+written ==");
    for f in snap.files.iter().take(10) {
        println!("{:>9.2} MB  {}", mb(f.bytes), f.path);
    }

    println!(
        "\n(durations dropped: {} negative [wrong-clock symptom], {} longer than 1 s [bad InitialTime])",
        iv.dur_negative, iv.dur_implausible
    );
    let ms = |ticks: i128| ticks as f64 * 1000.0 / freq as f64;
    // Machine-readable totals for comparing against the Windows \Processor % DPC/Interrupt counters.
    let total_dpc: i128 = iv.dpc_by_routine.values().map(|a| a.sum_ticks).sum();
    let total_isr: i128 = iv.isr_by_routine.values().map(|a| a.sum_ticks).sum();
    println!("TOTAL_DPC_MS {:.2} over {secs} s", ms(total_dpc));
    println!("TOTAL_ISR_MS {:.2} over {secs} s", ms(total_isr));

    println!("\n== DPC / ISR time by driver (QPC {freq} Hz) ==");
    println!(
        "{:<28} {:>9} {:>11} {:>9}   {:>9} {:>11} {:>9}",
        "driver", "DPC calls", "DPC ms", "max us", "ISR calls", "ISR ms", "max us"
    );
    for d in snap.drivers.iter().take(10) {
        println!(
            "{:<28} {:>9} {:>11.2} {:>9.1}   {:>9} {:>11.2} {:>9.1}",
            d.name,
            d.dpc_calls,
            d.dpc_time_us / 1000.0,
            d.dpc_max_us,
            d.isr_calls,
            d.isr_time_us / 1000.0,
            d.isr_max_us
        );
    }
}
