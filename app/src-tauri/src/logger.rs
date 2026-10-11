//! The optional activity log: a JSON-lines file the user switches on.
//!
//! Written by the app, not the collector: the collector is elevated and must not write into the
//! user's folders (the temp-file issue fixed in beta.3), and the app keeps sampling when its window
//! is minimised, which a web view's timers do not. Off whenever the app starts; nothing is recorded
//! until the user turns it on.
//!
//! One file per UTC day, `proclens-YYYY-MM-DD.jsonl`, in the log folder. Every line is a complete
//! JSON object with a schema version `v`. Times are UTC (RFC 3339) so a file is unambiguous when
//! shared. Each sample holds summary numbers plus the top few offenders; it contains process names,
//! file paths and driver names, which is why the UI says so next to the switch. It never contains
//! network addresses.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use proclens_proto::{ActivitySnapshot, MemoryStats, ProcessList};
use serde::Serialize;
use serde_json::{json, Value};

use crate::network::NetworkSnapshot;

/// Version of the line format. Bump it when a field changes meaning.
pub const SCHEMA: u32 = 1;
const TOP: usize = 5;
/// Files older than this many days (by the date in their name) are deleted.
pub const KEEP_DAYS: i64 = 28;
/// All log files together never exceed this; the oldest go first. A day of 5-second samples is
/// about 48 MB, so this is what keeps the 28 days within reach for every interval but the shortest.
pub const CAP_BYTES: u64 = 1024 * 1024 * 1024;
pub const MIN_INTERVAL_S: u64 = 5;
pub const MAX_INTERVAL_S: u64 = 60;
pub const DEFAULT_INTERVAL_S: u64 = 30;

// ---------------------------------------------------------------------------------------------
// Time: no date crate, so the few calendar functions needed are here, with tests.
// ---------------------------------------------------------------------------------------------

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn unix_secs(t: SystemTime) -> (u64, u32) {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    (d.as_secs(), d.subsec_millis())
}

/// `2026-10-11T13:40:05.123Z`.
pub fn rfc3339_utc(t: SystemTime) -> String {
    let (secs, ms) = unix_secs(t);
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Whole days since the epoch (the UTC date), used to start a new file at midnight UTC.
fn day_number(t: SystemTime) -> i64 {
    (unix_secs(t).0 / 86_400) as i64
}

pub fn file_name_for_day(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("proclens-{y:04}-{m:02}-{d:02}.jsonl")
}

/// The day number in a log file name, or None for any file that is not one of ours.
pub fn day_of_file_name(name: &str) -> Option<i64> {
    let stem = name.strip_prefix("proclens-")?.strip_suffix(".jsonl")?;
    let mut parts = stem.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (i64, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day = days_from_civil(y, m, d);
    (file_name_for_day(day) == name).then_some(day) // rejects 2026-02-31 and the like
}

// ---------------------------------------------------------------------------------------------
// Retention.
// ---------------------------------------------------------------------------------------------

/// Which of our files to delete: anything older than `keep_days` before `today`, then the oldest
/// remaining until the total is within `cap`. `files` is (name, size); other files are never chosen.
pub fn plan_prune(files: &[(String, u64)], today: i64, keep_days: i64, cap: u64) -> Vec<String> {
    let mut ours: Vec<(i64, &str, u64)> =
        files.iter().filter_map(|(n, s)| day_of_file_name(n).map(|d| (d, n.as_str(), *s))).collect();
    ours.sort();
    let mut doomed: Vec<String> = ours.iter().filter(|(d, _, _)| *d < today - keep_days).map(|(_, n, _)| n.to_string()).collect();
    let mut total: u64 = ours.iter().filter(|(_, n, _)| !doomed.iter().any(|x| x == n)).map(|(_, _, s)| *s).sum();
    for (_, name, size) in &ours {
        if total <= cap {
            break;
        }
        if !doomed.iter().any(|x| x == name) {
            doomed.push(name.to_string());
            total = total.saturating_sub(*size);
        }
    }
    doomed
}

fn prune_dir(dir: &Path, today: i64) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let files: Vec<(String, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            md.is_file().then(|| (e.file_name().to_string_lossy().into_owned(), md.len()))
        })
        .collect();
    for name in plan_prune(&files, today, KEEP_DAYS, CAP_BYTES) {
        let _ = fs::remove_file(dir.join(name));
    }
}

// ---------------------------------------------------------------------------------------------
// What goes in a sample.
// ---------------------------------------------------------------------------------------------

/// Disk, file and driver activity summed over every snapshot since the previous sample, so a sample
/// covers its whole interval and not just the last two seconds.
#[derive(Default)]
struct ActivityAcc {
    ms: u64,
    logical_cpus: u32,
    snapshots: u32,
    procs: HashMap<(u32, String), (u64, u64)>,
    files: HashMap<String, u64>,
    drivers: HashMap<String, (f64, f64)>,
    events_lost: u32,
    unresolved_bytes: u64,
    file_bytes: u64,
}

impl ActivityAcc {
    fn add(&mut self, a: &ActivitySnapshot) {
        self.ms += u64::from(a.interval_ms);
        self.logical_cpus = a.logical_cpus;
        self.snapshots += 1;
        for p in &a.processes {
            let e = self.procs.entry((p.pid, p.name.clone())).or_default();
            e.0 += p.disk_read_bytes + p.disk_write_bytes;
            e.1 += p.file_read_bytes + p.file_write_bytes;
        }
        for f in &a.files {
            *self.files.entry(f.path.clone()).or_default() += f.bytes;
        }
        for d in &a.drivers {
            let e = self.drivers.entry(d.name.clone()).or_default();
            e.0 += d.dpc_time_us;
            e.1 += d.isr_time_us;
        }
        self.events_lost = self.events_lost.max(a.health.events_lost);
        self.unresolved_bytes += a.health.unresolved_file_bytes;
        self.file_bytes += a.health.file_bytes_total;
    }

    /// The summed activity as a JSON value, or None if nothing arrived. Resets the totals.
    fn take(&mut self) -> Option<Value> {
        let acc = std::mem::take(self);
        if acc.snapshots == 0 {
            return None;
        }
        let mut procs: Vec<_> = acc.procs.into_iter().collect();
        procs.sort_by_key(|(_, (disk, file))| std::cmp::Reverse(disk + file));
        let mut files: Vec<_> = acc.files.into_iter().collect();
        files.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
        let mut drivers: Vec<_> = acc.drivers.into_iter().collect();
        drivers.sort_by(|a, b| (b.1 .0 + b.1 .1).total_cmp(&(a.1 .0 + a.1 .1)));
        let driver_us: f64 = drivers.iter().map(|(_, (d, i))| d + i).sum();
        let capacity_us = acc.ms as f64 * 1000.0 * f64::from(acc.logical_cpus.max(1));
        Some(json!({
            "seconds": acc.ms as f64 / 1000.0,
            "top_processes": procs.iter().take(TOP).map(|((pid, name), (disk, file))|
                json!({"name": name, "pid": pid, "disk_bytes": disk, "file_bytes": file})).collect::<Vec<_>>(),
            "top_files": files.iter().take(TOP).map(|(p, b)| json!({"path": p, "bytes": b})).collect::<Vec<_>>(),
            "top_drivers": drivers.iter().take(TOP).map(|(n, (d, i))|
                json!({"name": n, "dpc_ms": round1(d / 1000.0), "isr_ms": round1(i / 1000.0)})).collect::<Vec<_>>(),
            "driver_cpu_percent": if capacity_us > 0.0 { round1(driver_us / capacity_us * 100.0) } else { 0.0 },
            "events_lost": acc.events_lost,
            "unresolved_file_bytes": acc.unresolved_bytes,
            "file_bytes_total": acc.file_bytes,
        }))
    }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn memory_json(m: &MemoryStats) -> Value {
    let in_use = m.lists.as_ref().map_or(m.physical_total.saturating_sub(m.physical_available), |l| l.in_use);
    json!({
        "physical_total": m.physical_total,
        "physical_available": m.physical_available,
        "in_use": in_use,
        "commit_total": m.commit_total,
        "commit_limit": m.commit_limit,
        "kernel_nonpaged": m.kernel_nonpaged,
        "handles": m.handle_count,
        "processes": m.process_count,
        "threads": m.thread_count,
    })
}

fn top_processes_json(p: &ProcessList) -> (Value, Value) {
    let mut by_mem: Vec<_> = p.processes.iter().collect();
    by_mem.sort_by_key(|x| std::cmp::Reverse(x.working_set));
    let mut by_cpu: Vec<_> = p.processes.iter().collect();
    by_cpu.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
    (
        by_mem.iter().take(TOP).map(|x| json!({"name": x.name, "pid": x.pid, "working_set": x.working_set})).collect(),
        by_cpu
            .iter()
            .take(TOP)
            .filter(|x| x.cpu_percent > 0.0)
            .map(|x| json!({"name": x.name, "pid": x.pid, "cpu_percent": round1(f64::from(x.cpu_percent))}))
            .collect(),
    )
}

/// Bytes per second for one adapter between two readings; None across a counter reset or with no
/// time between them.
fn rates(prev: &crate::network::Adapter, cur: &crate::network::Adapter, ms: u64) -> Option<(f64, f64)> {
    if ms == 0 || cur.rx_bytes < prev.rx_bytes || cur.tx_bytes < prev.tx_bytes {
        return None;
    }
    let s = ms as f64 / 1000.0;
    Some(((cur.rx_bytes - prev.rx_bytes) as f64 / s, (cur.tx_bytes - prev.tx_bytes) as f64 / s))
}

fn network_json(prev: Option<&NetworkSnapshot>, cur: &NetworkSnapshot) -> Value {
    let ms = prev.map_or(0, |p| cur.taken_ms.saturating_sub(p.taken_ms));
    let adapters: Vec<Value> = cur
        .adapters
        .iter()
        .filter(|a| a.primary)
        .map(|a| {
            let r = prev.and_then(|p| p.adapters.iter().find(|b| b.luid == a.luid)).and_then(|b| rates(b, a, ms));
            json!({
                "name": a.name,
                "up": a.oper_status == 1,
                "rx_bytes_per_s": r.map(|x| x.0.round()),
                "tx_bytes_per_s": r.map(|x| x.1.round()),
                "rx_total": a.rx_bytes,
                "tx_total": a.tx_bytes,
                "errors_total": a.rx_errors + a.tx_errors,
                "dropped_total": a.rx_discards + a.tx_discards,
            })
        })
        .collect();
    json!({ "adapters": adapters })
}

// ---------------------------------------------------------------------------------------------
// The logger.
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LogStatus {
    pub enabled: bool,
    pub interval_s: u64,
    /// The folder, so the UI can say where the files are.
    pub dir: String,
    /// The file being written today, when recording.
    pub file: Option<String>,
    /// Lines written since logging was switched on.
    pub lines: u64,
    pub error: Option<String>,
    /// Retention rules, so the UI states them from one place.
    pub keep_days: i64,
    pub cap_mb: u64,
}

struct OpenLog {
    day: i64,
    file: File,
    name: String,
}

struct State {
    enabled: bool,
    interval_s: u64,
    out: Option<OpenLog>,
    due: Option<Instant>,
    lines: u64,
    error: Option<String>,
    collector: bool,
    memory: Option<MemoryStats>,
    processes: Option<ProcessList>,
    acc: ActivityAcc,
    prev_net: Option<NetworkSnapshot>,
}

pub struct Logger {
    dir: PathBuf,
    app_version: String,
    state: Mutex<State>,
}

impl Logger {
    pub fn new(dir: PathBuf, app_version: &str) -> Self {
        Logger {
            dir,
            app_version: app_version.to_string(),
            state: Mutex::new(State {
                enabled: false,
                interval_s: DEFAULT_INTERVAL_S,
                out: None,
                due: None,
                lines: 0,
                error: None,
                collector: false,
                memory: None,
                processes: None,
                acc: ActivityAcc::default(),
                prev_net: None,
            }),
        }
    }

    /// `%LOCALAPPDATA%\ProcLens\logs`.
    pub fn default_dir() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("ProcLens").join("logs"))
    }

    fn status_of(&self, st: &State) -> LogStatus {
        LogStatus {
            enabled: st.enabled,
            interval_s: st.interval_s,
            dir: self.dir.to_string_lossy().into_owned(),
            file: st.out.as_ref().map(|o| o.name.clone()),
            lines: st.lines,
            error: st.error.clone(),
            keep_days: KEEP_DAYS,
            cap_mb: CAP_BYTES / (1024 * 1024),
        }
    }

    pub fn status(&self) -> LogStatus {
        let st = self.state.lock().unwrap();
        self.status_of(&st)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // --- data the sampler draws on -----------------------------------------------------------

    pub fn feed_memory(&self, m: &MemoryStats) {
        let mut st = self.state.lock().unwrap();
        if st.enabled {
            st.memory = Some(m.clone());
        }
    }

    pub fn feed_processes(&self, p: &ProcessList) {
        let mut st = self.state.lock().unwrap();
        if st.enabled {
            st.processes = Some(p.clone());
        }
    }

    pub fn feed_activity(&self, a: &ActivitySnapshot) {
        let mut st = self.state.lock().unwrap();
        if st.enabled {
            st.acc.add(a);
        }
    }

    /// The collector connected or went away. Its data is dropped on disconnect so a stale reading
    /// is never logged as current.
    pub fn set_collector(&self, connected: bool) {
        let mut st = self.state.lock().unwrap();
        st.collector = connected;
        if !connected {
            st.memory = None;
            st.processes = None;
            st.acc = ActivityAcc::default();
        }
    }

    // --- switching on and off ----------------------------------------------------------------

    /// Turns logging on or off (and sets the interval, clamped). Returns the new status.
    pub fn set(&self, enabled: bool, interval_s: Option<u64>, now: SystemTime, at: Instant) -> LogStatus {
        let mut st = self.state.lock().unwrap();
        if let Some(i) = interval_s {
            st.interval_s = i.clamp(MIN_INTERVAL_S, MAX_INTERVAL_S);
        }
        if enabled && !st.enabled {
            st.enabled = true;
            st.error = None;
            st.lines = 0;
            st.acc = ActivityAcc::default();
            st.prev_net = None;
            st.due = Some(at + Duration::from_secs(st.interval_s));
            if let Err(e) = fs::create_dir_all(&self.dir) {
                self.fail(&mut st, format!("could not create the log folder: {e}"));
            } else {
                prune_dir(&self.dir, day_number(now));
                let line = json!({"v": SCHEMA, "event": "start", "t": rfc3339_utc(now),
                    "app_version": self.app_version, "interval_s": st.interval_s});
                self.write(&mut st, &line, now);
            }
        } else if !enabled && st.enabled {
            let line = json!({"v": SCHEMA, "event": "stop", "t": rfc3339_utc(now)});
            self.write(&mut st, &line, now);
            st.enabled = false;
            st.out = None;
            st.due = None;
            st.memory = None;
            st.processes = None;
            st.acc = ActivityAcc::default();
        } else if enabled {
            // Already on: just a new interval, applied from the next sample.
            st.due = Some(at + Duration::from_secs(st.interval_s));
        }
        self.status_of(&st)
    }

    fn fail(&self, st: &mut State, msg: String) {
        st.error = Some(msg);
        st.enabled = false;
        st.out = None;
        st.due = None;
    }

    /// Appends one line to today's file, opening a new file after midnight UTC. A write error stops
    /// logging and is reported, rather than retried forever.
    fn write(&self, st: &mut State, line: &Value, now: SystemTime) {
        let day = day_number(now);
        if st.out.as_ref().is_none_or(|o| o.day != day) {
            let name = file_name_for_day(day);
            match OpenOptions::new().create(true).append(true).open(self.dir.join(&name)) {
                Ok(file) => {
                    if st.out.is_some() {
                        prune_dir(&self.dir, day);
                    }
                    st.out = Some(OpenLog { day, file, name });
                }
                Err(e) => return self.fail(st, format!("could not open {name}: {e}")),
            }
        }
        let out = st.out.as_mut().expect("opened above");
        let mut text = line.to_string();
        text.push('\n');
        match out.file.write_all(text.as_bytes()).and_then(|()| out.file.flush()) {
            Ok(()) => st.lines += 1,
            Err(e) => self.fail(st, format!("could not write to the log: {e}")),
        }
    }

    // --- sampling ----------------------------------------------------------------------------

    /// True when a sample is due; the caller then reads the network and calls [`sample`].
    pub fn due(&self, at: Instant) -> bool {
        let st = self.state.lock().unwrap();
        st.enabled && st.due.is_some_and(|d| at >= d)
    }

    /// Writes one sample line. Returns the new status.
    pub fn sample(&self, now: SystemTime, at: Instant, net: Option<NetworkSnapshot>) -> LogStatus {
        let mut st = self.state.lock().unwrap();
        if !st.enabled {
            return self.status_of(&st);
        }
        st.due = Some(at + Duration::from_secs(st.interval_s));
        let (top_mem, top_cpu) = st.processes.as_ref().map_or((Value::Null, Value::Null), top_processes_json);
        let network = net.as_ref().map_or(Value::Null, |cur| network_json(st.prev_net.as_ref(), cur));
        let activity = st.acc.take().unwrap_or(Value::Null);
        let line = json!({
            "v": SCHEMA,
            "t": rfc3339_utc(now),
            "interval_s": st.interval_s,
            "collector": st.collector,
            "memory": st.memory.as_ref().map_or(Value::Null, memory_json),
            "top_memory": top_mem,
            "top_cpu": top_cpu,
            "activity": activity,
            "network": network,
        });
        if net.is_some() {
            st.prev_net = net;
        }
        self.write(&mut st, &line, now);
        self.status_of(&st)
    }

    /// Writes an extra event line (for example a detected anomaly). Ignored when logging is off.
    pub fn event(&self, now: SystemTime, kind: &str, detail: Value) {
        let mut st = self.state.lock().unwrap();
        if st.enabled {
            let line = json!({"v": SCHEMA, "event": kind, "t": rfc3339_utc(now), "detail": detail});
            self.write(&mut st, &line, now);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------------------------

#[tauri::command]
pub fn log_status(logger: tauri::State<'_, Logger>) -> LogStatus {
    logger.status()
}

#[tauri::command]
pub fn log_set(
    app: tauri::AppHandle,
    logger: tauri::State<'_, Logger>,
    enabled: bool,
    interval_s: Option<u64>,
) -> LogStatus {
    use tauri::Emitter;
    let s = logger.set(enabled, interval_s, SystemTime::now(), Instant::now());
    let _ = app.emit("log-status", s.clone());
    s
}

/// Opens the log folder in Explorer. Only ever the folder this logger writes to.
#[tauri::command]
pub fn log_open_folder(logger: tauri::State<'_, Logger>) -> Result<(), String> {
    let dir = logger.dir().to_path_buf();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let r = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(dir.as_os_str()), None, None, SW_SHOWNORMAL) };
        if (r.0 as isize) > 32 {
            Ok(())
        } else {
            Err("could not open the folder".into())
        }
    }
    #[cfg(not(windows))]
    Err("opening folders is only available on Windows".into())
}

/// Called once a second by the sampler thread started in `run`.
pub fn tick(app: &tauri::AppHandle, logger: &Logger) {
    use tauri::Emitter;
    let at = Instant::now();
    if !logger.due(at) {
        return;
    }
    let net = crate::network::snapshot().ok();
    let s = logger.sample(SystemTime::now(), at, net);
    let _ = app.emit("log-status", s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::{Adapter, Medium};
    use proclens_proto::{ActivityHealth, DriverActivity, FileActivity, MemoryLists, ProcessActivity, ProcessInfo};

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("proclens-logger-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn rfc3339_known_instants() {
        assert_eq!(rfc3339_utc(at(0)), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_utc(at(1_700_000_000)), "2023-11-14T22:13:20.000Z");
        assert_eq!(rfc3339_utc(at(1_709_164_800)), "2024-02-29T00:00:00.000Z"); // a leap day
        assert_eq!(rfc3339_utc(at(951_782_400)), "2000-02-29T00:00:00.000Z"); // divisible by 400
        assert_eq!(rfc3339_utc(at(4_102_444_799)), "2099-12-31T23:59:59.000Z");
        assert_eq!(rfc3339_utc(UNIX_EPOCH + Duration::from_millis(1_234)), "1970-01-01T00:00:01.234Z");
    }

    #[test]
    fn day_numbers_and_file_names_round_trip() {
        assert_eq!(day_number(at(1_709_164_800)), 19_782);
        assert_eq!(file_name_for_day(19_782), "proclens-2024-02-29.jsonl");
        assert_eq!(day_of_file_name("proclens-2024-02-29.jsonl"), Some(19_782));
        assert_eq!(days_from_civil(2026, 10, 11), 20_737);
        for day in [-1, 0, 11_016, 19_782, 20_737, 40_000] {
            assert_eq!(day_of_file_name(&file_name_for_day(day)), Some(day), "day {day}");
        }
    }

    #[test]
    fn only_our_exact_file_names_are_recognised() {
        for bad in [
            "proclens-2024-02-30.jsonl", "proclens-2023-02-29.jsonl", "proclens-2024-13-01.jsonl", "proclens-24-02-29.jsonl",
            "proclens-2024-02-29.jsonl.bak", "notes.jsonl", "proclens-2024-02-29-extra.jsonl", "PROCLENS-2024-02-29.jsonl",
            "proclens-2024-2-29.jsonl", "",
        ] {
            assert_eq!(day_of_file_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn prune_plan_drops_old_files_then_enforces_the_cap_and_never_touches_others() {
        let today = 20_737;
        let f = |d: i64, s: u64| (file_name_for_day(d), s);
        let files = vec![
            f(today - 60, 10),
            f(today - KEEP_DAYS - 1, 10),
            f(today - KEEP_DAYS, 10), // exactly KEEP_DAYS old: kept
            f(today - 3, 60),
            f(today, 60),
            ("my-notes.txt".to_string(), 9_999_999),
            ("proclens-backup.zip".to_string(), 9_999_999),
        ];
        let doomed = plan_prune(&files, today, KEEP_DAYS, 1_000);
        assert_eq!(doomed, vec![file_name_for_day(today - 60), file_name_for_day(today - KEEP_DAYS - 1)]);
        // A tight cap removes the oldest survivors first, and still only our files. The three newest
        // files total 130 bytes: one removal (the oldest, 10) gets that under 125.
        let doomed = plan_prune(&files, today, KEEP_DAYS, 125);
        assert_eq!(
            doomed,
            vec![file_name_for_day(today - 60), file_name_for_day(today - KEEP_DAYS - 1), file_name_for_day(today - KEEP_DAYS)]
        );
        let doomed = plan_prune(&files, today, KEEP_DAYS, 0);
        assert_eq!(doomed.len(), 5);
        assert!(!doomed.iter().any(|n| n == "my-notes.txt" || n == "proclens-backup.zip"));
    }

    fn adapter(luid: u64, name: &str, rx: u64, tx: u64) -> Adapter {
        Adapter {
            luid, name: name.into(), description: "secret-model".into(), medium: Medium::Ethernet, hardware: true,
            primary: true, oper_status: 1, media_connected: Some(true), rx_speed_bps: None, tx_speed_bps: None,
            rx_bytes: rx, tx_bytes: tx, rx_packets: 0, tx_packets: 0, rx_errors: 1, tx_errors: 2, rx_discards: 3, tx_discards: 4,
        }
    }

    fn net(ms: u64, rx: u64, tx: u64) -> NetworkSnapshot {
        let mut soft = adapter(9, "WAN Miniport", 0, 0);
        soft.primary = false;
        NetworkSnapshot { taken_ms: ms, adapters: vec![adapter(1, "Ethernet 2", rx, tx), soft] }
    }

    fn memory() -> MemoryStats {
        MemoryStats {
            physical_total: 32_000, physical_available: 12_000, system_cache: 0, kernel_nonpaged: 5, kernel_paged: 0,
            commit_total: 20_000, commit_limit: 60_000, handle_count: 1, process_count: 2, thread_count: 3,
            lists: Some(MemoryLists { free_zero: 1, standby: 2, modified: 3, bad: 0, in_use: 17_000 }),
        }
    }

    fn procs() -> ProcessList {
        let p = |pid, name: &str, ws, cpu| ProcessInfo { pid, name: name.into(), working_set: ws, virtual_size: 0, cpu_percent: cpu };
        ProcessList {
            processes: vec![p(1, "small.exe", 10, 0.0), p(2, "big.exe", 9_000, 1.0), p(3, "busy.exe", 500, 55.56), p(4, "mid.exe", 700, 0.0)],
            total_working_set: 10_210,
        }
    }

    fn activity() -> ActivitySnapshot {
        ActivitySnapshot {
            interval_ms: 2000, logical_cpus: 4,
            processes: vec![ProcessActivity {
                pid: 7, name: "disky.exe".into(), disk_read_bytes: 100, disk_write_bytes: 50, disk_ops: 1, disk_latency_us: None,
                file_read_bytes: 10, file_write_bytes: 5, file_ops: 1,
            }],
            files: vec![FileActivity { path: r"C:\data\big.bin".into(), bytes: 1_000 }],
            drivers: vec![DriverActivity { name: "nvlddmkm.sys".into(), dpc_calls: 1, dpc_time_us: 4_000.0, dpc_max_us: 1.0, isr_calls: 1, isr_time_us: 4_000.0, isr_max_us: 1.0 }],
            health: ActivityHealth { events_lost: 0, buffers_lost: 0, io_failed: 0, durations_dropped: 0, unresolved_file_bytes: 7, file_bytes_total: 1_000 },
        }
    }

    fn read_lines(dir: &Path) -> Vec<Value> {
        let mut out = Vec::new();
        let mut names: Vec<_> = fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        names.sort();
        for p in names {
            for l in fs::read_to_string(p).unwrap().lines() {
                out.push(serde_json::from_str(l).expect("every line is JSON"));
            }
        }
        out
    }

    #[test]
    fn nothing_is_written_until_switched_on() {
        let dir = tmp("off");
        let lg = Logger::new(dir.clone(), "9.9.9");
        let t0 = Instant::now();
        assert!(!lg.due(t0 + Duration::from_secs(10_000)));
        lg.feed_memory(&memory());
        assert_eq!(lg.status().lines, 0);
        assert!(!dir.exists(), "not even the folder is created");
    }

    #[test]
    fn a_session_writes_start_samples_and_stop_with_the_documented_content() {
        let dir = tmp("session");
        let lg = Logger::new(dir.clone(), "9.9.9");
        let t0 = Instant::now();
        let s = lg.set(true, Some(30), at(1_700_000_000), t0);
        assert!(s.enabled && s.lines == 1 && s.file.as_deref() == Some("proclens-2023-11-14.jsonl"), "{s:?}");
        assert!(!lg.due(t0 + Duration::from_secs(29)));
        assert!(lg.due(t0 + Duration::from_secs(30)));

        lg.set_collector(true);
        lg.feed_memory(&memory());
        lg.feed_processes(&procs());
        lg.feed_activity(&activity());
        lg.feed_activity(&activity());
        lg.sample(at(1_700_000_030), t0 + Duration::from_secs(30), Some(net(1_000, 1_000, 2_000)));
        lg.sample(at(1_700_000_060), t0 + Duration::from_secs(60), Some(net(31_000, 31_000, 62_000)));
        let s = lg.set(false, None, at(1_700_000_061), t0 + Duration::from_secs(61));
        assert!(!s.enabled && s.file.is_none());

        let lines = read_lines(&dir);
        assert_eq!(lines.len(), 4, "{lines:#?}");
        assert_eq!(lines[0]["event"], "start");
        assert_eq!(lines[0]["app_version"], "9.9.9");
        assert_eq!(lines[3]["event"], "stop");

        let a = &lines[1];
        assert_eq!(a["v"], 1);
        assert_eq!(a["t"], "2023-11-14T22:13:50.000Z");
        assert_eq!(a["collector"], true);
        assert_eq!(a["memory"]["in_use"], 17_000);
        assert_eq!(a["top_memory"][0]["name"], "big.exe");
        assert_eq!(a["top_cpu"][0]["name"], "busy.exe");
        assert_eq!(a["top_cpu"][0]["cpu_percent"], 55.6);
        assert_eq!(a["top_cpu"].as_array().unwrap().len(), 2, "processes at 0% are not 'top CPU'");
        assert_eq!(a["activity"]["seconds"], 4.0, "two 2-second snapshots were summed");
        assert_eq!(a["activity"]["top_processes"][0]["disk_bytes"], 300);
        assert_eq!(a["activity"]["top_files"][0]["bytes"], 2_000);
        assert_eq!(a["activity"]["top_drivers"][0]["dpc_ms"], 8.0);
        // 16 ms of driver time over 4 s x 4 CPUs.
        assert_eq!(a["activity"]["driver_cpu_percent"], 0.1);
        assert!(a["network"]["adapters"][0]["rx_bytes_per_s"].is_null(), "no rate from a first reading");
        assert_eq!(a["network"]["adapters"].as_array().unwrap().len(), 1, "software adapters are left out");

        let b = &lines[2];
        assert!(b["activity"].is_null(), "the activity totals reset after each sample");
        assert_eq!(b["network"]["adapters"][0]["rx_bytes_per_s"], 1_000.0);
        assert_eq!(b["network"]["adapters"][0]["tx_bytes_per_s"], 2_000.0);
        assert_eq!(b["network"]["adapters"][0]["dropped_total"], 7);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_never_contains_an_address_or_adapter_description() {
        let dir = tmp("privacy");
        let lg = Logger::new(dir.clone(), "1");
        let t0 = Instant::now();
        lg.set(true, None, at(1_700_000_000), t0);
        lg.sample(at(1_700_000_030), t0, Some(net(1, 1, 1)));
        let text = fs::read_to_string(dir.join("proclens-2023-11-14.jsonl")).unwrap();
        assert!(!text.contains("secret-model"), "driver descriptions and MACs stay out");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_collector_that_goes_away_leaves_no_stale_numbers() {
        let dir = tmp("stale");
        let lg = Logger::new(dir.clone(), "1");
        let t0 = Instant::now();
        lg.set(true, None, at(1_700_000_000), t0);
        lg.set_collector(true);
        lg.feed_memory(&memory());
        lg.feed_processes(&procs());
        lg.set_collector(false);
        lg.sample(at(1_700_000_030), t0, None);
        let lines = read_lines(&dir);
        assert_eq!(lines[1]["collector"], false);
        assert!(lines[1]["memory"].is_null() && lines[1]["top_memory"].is_null());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn counters_that_reset_give_no_rate_not_a_nonsense_one() {
        let (a, b) = (adapter(1, "x", 5_000, 5_000), adapter(1, "x", 10, 10));
        assert_eq!(rates(&a, &b, 1_000), None);
        assert_eq!(rates(&b, &a, 0), None);
        assert_eq!(rates(&b, &a, 1_000), Some((4_990.0, 4_990.0)));
    }

    #[test]
    fn the_file_rolls_over_at_midnight_utc() {
        let dir = tmp("midnight");
        let lg = Logger::new(dir.clone(), "1");
        let t0 = Instant::now();
        let before = 1_700_006_399; // 2023-11-14T23:59:59Z
        lg.set(true, None, at(before), t0);
        lg.sample(at(before + 1), t0, None);
        let s = lg.status();
        assert_eq!(s.file.as_deref(), Some("proclens-2023-11-15.jsonl"));
        assert!(dir.join("proclens-2023-11-14.jsonl").exists() && dir.join("proclens-2023-11-15.jsonl").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_on_prunes_only_our_old_files() {
        let dir = tmp("prune");
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("proclens-2023-01-01.jsonl");
        let other = dir.join("keep-me.txt");
        fs::write(&old, "x").unwrap();
        fs::write(&other, "x").unwrap();
        let lg = Logger::new(dir.clone(), "1");
        lg.set(true, None, at(1_700_000_000), Instant::now());
        assert!(!old.exists());
        assert!(other.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_cannot_be_created_is_reported_and_logging_stays_off() {
        let dir = tmp("blocked");
        fs::write(&dir, "a file where the folder should be").unwrap();
        let lg = Logger::new(dir.join("logs"), "1");
        let s = lg.set(true, None, at(1_700_000_000), Instant::now());
        assert!(!s.enabled);
        assert!(s.error.as_deref().is_some_and(|e| e.contains("log folder")), "{s:?}");
        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn the_interval_is_clamped_and_can_change_while_on() {
        let dir = tmp("interval");
        let lg = Logger::new(dir.clone(), "1");
        let t0 = Instant::now();
        assert_eq!(lg.set(true, Some(1), at(1_700_000_000), t0).interval_s, MIN_INTERVAL_S);
        assert_eq!(lg.set(true, Some(999_999), at(1_700_000_001), t0).interval_s, MAX_INTERVAL_S);
        assert_eq!((MIN_INTERVAL_S, MAX_INTERVAL_S), (5, 60), "the choices offered run from 5 s to 1 minute");
        assert_eq!(lg.set(true, Some(45), at(1_700_000_002), t0).interval_s, 45);
        assert_eq!(lg.status().lines, 1, "changing the interval does not write another start line");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_events_are_logged_only_while_on() {
        let dir = tmp("event");
        let lg = Logger::new(dir.clone(), "1");
        lg.event(at(1_700_000_000), "anomaly", json!({"x": 1}));
        assert!(!dir.exists());
        lg.set(true, None, at(1_700_000_000), Instant::now());
        lg.event(at(1_700_000_001), "anomaly", json!({"x": 1}));
        assert_eq!(read_lines(&dir)[1]["detail"]["x"], 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
