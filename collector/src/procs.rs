//! Process list via sysinfo (baseline to compare against Task Manager).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use proclens_proto::{ProcessInfo, ProcessList};
use sysinfo::{ProcessesToUpdate, System};

/// Shared by every client connection so N subscribers cost one refresh, not N, and so CPU
/// percentages are measured over a consistent interval.
pub struct Sampler {
    inner: Mutex<(System, Option<Instant>)>,
}

/// Samples closer together than this reuse the previous data (sysinfo's CPU% needs a real interval).
const MIN_REFRESH: Duration = Duration::from_millis(500);

impl Sampler {
    pub fn new() -> Self {
        Self { inner: Mutex::new((System::new(), None)) }
    }

    pub fn processes(&self) -> ProcessList {
        let mut guard = self.inner.lock().unwrap();
        let (sys, last) = &mut *guard;
        if last.map_or(true, |t| t.elapsed() >= MIN_REFRESH) {
            sys.refresh_processes(ProcessesToUpdate::All, true);
            *last = Some(Instant::now());
        }
        let processes: Vec<ProcessInfo> = sys
            .processes()
            .values()
            .map(|p| ProcessInfo {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                working_set: p.memory(),
                virtual_size: p.virtual_memory(),
                cpu_percent: p.cpu_usage(),
            })
            .collect();
        let total_working_set = processes.iter().map(|p| p.working_set).sum();
        ProcessList { processes, total_working_set }
    }
}

pub fn run() {
    let sampler = Sampler::new();
    sampler.processes();
    std::thread::sleep(Duration::from_secs(1));
    let mut list = sampler.processes();

    println!(
        "{} processes; sum of working sets = {:.0} MB (shared pages are counted once per process)",
        list.processes.len(),
        list.total_working_set as f64 / 1048576.0
    );

    list.processes.sort_by_key(|p| std::cmp::Reverse(p.working_set));
    println!("\nTop 10 by working set:");
    println!("{:>7}  {:>10}  {:>10}  name", "pid", "WS MB", "virt MB");
    for p in list.processes.iter().take(10) {
        println!(
            "{:>7}  {:>10.1}  {:>10.1}  {}",
            p.pid,
            p.working_set as f64 / 1048576.0,
            p.virtual_size as f64 / 1048576.0,
            p.name
        );
    }

    list.processes.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
    println!("\nTop 10 by CPU over the last second (percent of one core):");
    for p in list.processes.iter().take(10) {
        println!("{:>7}  {:>6.1}%  {}", p.pid, p.cpu_percent, p.name);
    }
}
