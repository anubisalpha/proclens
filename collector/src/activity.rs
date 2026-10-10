//! Turns a raw trace [`Interval`] into the protocol's [`ActivitySnapshot`]: names, driver
//! attribution, drive letters, time units, and trimming to the most active entries.

use std::collections::{HashMap, HashSet};

use proclens_proto::{ActivityHealth, ActivitySnapshot, DriverActivity, FileActivity, ProcessActivity};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::QueryDosDeviceW;

use crate::trace::{owner, Interval, SessionHealth, UNRESOLVED};

const MAX_PROCESSES: usize = 100;
const MAX_FILES: usize = 60;
const MAX_DRIVERS: usize = 30;

/// `(\Device\HarddiskVolume3, C:)` for every mounted drive letter.
pub fn volume_map() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let drive = format!("{}:", letter as char);
        let wide: Vec<u16> = drive.encode_utf16().chain([0]).collect();
        let mut buf = [0u16; 512];
        let n = unsafe { QueryDosDeviceW(PCWSTR(wide.as_ptr()), Some(&mut buf)) } as usize;
        if n == 0 {
            continue;
        }
        // The result is a multi-string; the first entry is the current mapping.
        let first = buf[..n].split(|&c| c == 0).next().unwrap_or(&[]);
        let device = String::from_utf16_lossy(first);
        if device.starts_with(r"\Device\") {
            out.push((device, drive));
        }
    }
    out
}

fn friendly_path(volumes: &[(String, String)], nt: &str) -> String {
    for (device, drive) in volumes {
        // `get` (not a slice) so a non-ASCII path can't panic on a character boundary.
        if nt.get(..device.len()).is_some_and(|head| head.eq_ignore_ascii_case(device)) {
            let tail = &nt[device.len()..];
            // Whole component only: HarddiskVolume3 must not match HarddiskVolume30.
            if tail.is_empty() || tail.starts_with('\\') {
                return format!("{drive}{tail}");
            }
        }
    }
    nt.to_string()
}

pub fn build(
    iv: &Interval,
    names: &HashMap<u32, String>,
    health: Option<&SessionHealth>,
    qpc_freq: i64,
    drivers: &[(usize, String)],
    volumes: &[(String, String)],
) -> ActivitySnapshot {
    // DPC/ISR durations and the disk event's HighResResponseTime are both in QPC ticks
    // (Microsoft documents the latter as KeQueryPerformanceCounter units).
    let to_us = |ticks: f64| ticks * 1_000_000.0 / qpc_freq.max(1) as f64;

    // --- processes: union of disk and file activity ---
    let pids: HashSet<u32> = iv.disk_by_pid.keys().chain(iv.file_by_pid.keys()).copied().collect();
    let mut processes: Vec<ProcessActivity> = pids
        .into_iter()
        .map(|pid| {
            let d = iv.disk_by_pid.get(&pid);
            let f = iv.file_by_pid.get(&pid);
            ProcessActivity {
                pid,
                name: if pid == u32::MAX {
                    // The trace couldn't tell which process issued these requests.
                    "(unknown process)".to_string()
                } else {
                    names.get(&pid).cloned().unwrap_or_else(|| "(exited)".to_string())
                },
                disk_read_bytes: d.map_or(0, |a| a.read_bytes),
                disk_write_bytes: d.map_or(0, |a| a.write_bytes),
                disk_ops: d.map_or(0, |a| a.reads + a.writes),
                disk_latency_us: d
                    .filter(|a| a.resp_count > 0)
                    .map(|a| to_us(a.resp_ticks as f64 / a.resp_count as f64)),
                file_read_bytes: f.map_or(0, |a| a.read_bytes),
                file_write_bytes: f.map_or(0, |a| a.write_bytes),
                file_ops: f.map_or(0, |a| a.reads + a.writes),
            }
        })
        .collect();
    processes.sort_by_key(|p| {
        std::cmp::Reverse(p.disk_read_bytes + p.disk_write_bytes + p.file_read_bytes + p.file_write_bytes)
    });
    processes.truncate(MAX_PROCESSES);

    // --- files ---
    let mut files: Vec<FileActivity> = iv
        .file_bytes
        .iter()
        .filter(|(_, b)| **b > 0)
        .map(|(p, b)| FileActivity { path: friendly_path(volumes, p), bytes: *b })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.bytes));
    files.truncate(MAX_FILES);

    // --- drivers: DPC and ISR routines grouped by owning driver ---
    let mut by_driver: HashMap<&str, DriverActivity> = HashMap::new();
    let mut add = |routine: usize, a: &crate::trace::TimeAgg, isr: bool| {
        let name = owner(drivers, routine);
        let e = by_driver.entry(name).or_insert_with(|| DriverActivity {
            name: name.to_string(),
            dpc_calls: 0,
            dpc_time_us: 0.0,
            dpc_max_us: 0.0,
            isr_calls: 0,
            isr_time_us: 0.0,
            isr_max_us: 0.0,
        });
        let (time, max) = (to_us(a.sum_ticks as f64), to_us(a.max_ticks as f64));
        if isr {
            e.isr_calls += a.count;
            e.isr_time_us += time;
            e.isr_max_us = e.isr_max_us.max(max);
        } else {
            e.dpc_calls += a.count;
            e.dpc_time_us += time;
            e.dpc_max_us = e.dpc_max_us.max(max);
        }
    };
    for (r, a) in &iv.dpc_by_routine {
        add(*r, a, false);
    }
    for (r, a) in &iv.isr_by_routine {
        add(*r, a, true);
    }
    let mut drivers_out: Vec<DriverActivity> = by_driver.into_values().collect();
    drivers_out.sort_by(|a, b| {
        (b.dpc_time_us + b.isr_time_us).total_cmp(&(a.dpc_time_us + a.isr_time_us))
    });
    drivers_out.truncate(MAX_DRIVERS);

    let file_bytes_total: u64 = iv.file_bytes.values().sum();
    ActivitySnapshot {
        interval_ms: iv.elapsed.as_millis().min(u32::MAX as u128) as u32,
        logical_cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        processes,
        files,
        drivers: drivers_out,
        health: ActivityHealth {
            events_lost: health.map_or(0, |h| h.events_lost),
            buffers_lost: health.map_or(0, |h| h.log_buffers_lost + h.rt_buffers_lost),
            io_failed: iv.io_failed,
            durations_dropped: iv.dur_negative + iv.dur_implausible,
            unresolved_file_bytes: iv.file_bytes.get(UNRESOLVED).copied().unwrap_or(0),
            file_bytes_total,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nt_paths_become_drive_letters_only_on_a_whole_component() {
        let vols = vec![(r"\Device\HarddiskVolume3".to_string(), "C:".to_string())];
        assert_eq!(friendly_path(&vols, r"\Device\HarddiskVolume3\Users\me\a.txt"), r"C:\Users\me\a.txt");
        assert_eq!(friendly_path(&vols, r"\device\harddiskvolume3\x"), r"C:\x");
        // Volume 30 is a different device: must not be rewritten as C:0\...
        assert_eq!(friendly_path(&vols, r"\Device\HarddiskVolume30\x"), r"\Device\HarddiskVolume30\x");
        assert_eq!(friendly_path(&vols, "(something else)"), "(something else)");
    }

    #[test]
    fn snapshot_merges_disk_and_file_activity_and_trims() {
        use crate::trace::IoAgg;
        let mut iv = Interval::default();
        iv.elapsed = std::time::Duration::from_secs(2);
        iv.disk_by_pid.insert(10, IoAgg { reads: 2, read_bytes: 4096, resp_ticks: 400, resp_count: 2, ..Default::default() });
        iv.file_by_pid.insert(10, IoAgg { writes: 1, write_bytes: 100, ..Default::default() });
        iv.file_by_pid.insert(11, IoAgg { reads: 1, read_bytes: 7, ..Default::default() });
        iv.file_bytes.insert(UNRESOLVED.to_string(), 7);
        let names = HashMap::from([(10, "a.exe".to_string())]);
        let s = build(&iv, &names, None, 10_000_000, &[], &[]);
        assert_eq!(s.interval_ms, 2000);
        assert_eq!(s.processes.len(), 2);
        let a = s.processes.iter().find(|p| p.pid == 10).unwrap();
        assert_eq!((a.disk_read_bytes, a.file_write_bytes), (4096, 100));
        // 400 ticks over 2 requests = 200 ticks = 20 us at a 10 MHz counter
        assert!((a.disk_latency_us.unwrap() - 20.0).abs() < 1e-9);
        // The same ticks on a 3 MHz counter are a different time: latency must follow the counter.
        let slow = build(&iv, &names, None, 3_000_000, &[], &[]);
        let a = slow.processes.iter().find(|p| p.pid == 10).unwrap();
        assert!((a.disk_latency_us.unwrap() - 200.0 * 1e6 / 3e6).abs() < 1e-9);
        assert_eq!(s.processes.iter().find(|p| p.pid == 11).unwrap().name, "(exited)");
        assert_eq!(s.health.unresolved_file_bytes, 7);
    }
}
