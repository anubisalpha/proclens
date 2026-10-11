//! The optional connection log: which program talked to which address, and for how long.
//!
//! A separate file set from the activity log (`logger.rs`), switched on separately and off whenever
//! the app starts. It exists because the activity log never records network addresses; this one
//! does, so it has its own switch, its own folder entries (`proclens-connections-YYYY-MM-DD.jsonl`,
//! which the activity log's pruner cannot match, and the reverse) and its own retention.
//!
//! How it works: every few seconds the TCP/UDP owner tables (see `ports.rs`) are read and compared
//! with the previous reading. A connection or listener that is new gets remembered; one that has
//! gone gets one line written, with when it was first and last seen. So a line is written when a
//! session ENDS and is always complete. What this cannot see:
//! - sessions shorter than the interval (they may sit between two readings);
//! - the exact moment a session ended: `last_seen` is the last reading that still had it, so the
//!   true end is up to one interval later, and the true start up to one interval earlier;
//! - how many bytes a connection or program moved: the tables carry no counters, and per-process
//!   traffic needs the elevated kernel trace, which is not built yet. What IS written, at every
//!   check, is a `traffic` line: the whole PC's bytes in and out per network adapter since the
//!   previous check (the same counters as the Network tab). It says how busy the network was at
//!   that moment, not who was using it;
//! - sessions still open when the app is killed rather than closed (logging is switched off and
//!   every open session is written when the window closes, but a crash loses them).
//!
//! Left out on purpose: anything to or from a loopback address (programs talking to each other on
//! this PC), TCP connections that are already closing (TIME_WAIT and friends, which Windows keeps
//! listed after the program has let go), and listeners that only accept local connections.
//!
//! `new_peer` is true the first time this program has been seen talking to this remote address. The
//! set of pairs seen so far is kept in `proclens-connections-seen.json` (at most [`SEEN_CAP`]; the
//! oldest are forgotten first), so it survives restarts. On the very first run everything is new:
//! the `start` line says how many pairs were already known.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::{json, Value};

use crate::logger::{day_number, name_for_day, plan_prune_for, rfc3339_utc, unix_secs};
use crate::network::NetworkSnapshot;
use crate::ports::{Endpoint, Owner, PortsSnapshot, Proto};

/// Version of the line format. Bump it when a field changes meaning.
pub const SCHEMA: u32 = 1;
pub const FILE_PREFIX: &str = "proclens-connections-";
pub const SEEN_FILE: &str = "proclens-connections-seen.json";
/// Files older than this many days (by the date in their name) are deleted.
pub const KEEP_DAYS: i64 = 28;
/// All connection-log files together never exceed this; the oldest go first.
pub const CAP_BYTES: u64 = 256 * 1024 * 1024;
pub const MIN_INTERVAL_S: u64 = 5;
pub const MAX_INTERVAL_S: u64 = 60;
pub const DEFAULT_INTERVAL_S: u64 = 10;
/// How many (program, address) pairs are remembered between runs.
pub const SEEN_CAP: usize = 20_000;

// ---------------------------------------------------------------------------------------------
// What counts as a session.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A TCP connection to another machine.
    Connection,
    /// A TCP listener or bound UDP socket that accepts non-local traffic.
    Listener,
}

pub fn is_loopback(addr: &str) -> bool {
    addr.parse::<IpAddr>().is_ok_and(|a| a.is_loopback())
}

/// TCP states in which the program has let go and Windows is only tidying up.
fn is_ending(state: Option<&str>) -> bool {
    matches!(state, Some("time_wait" | "fin_wait_1" | "fin_wait_2" | "closing" | "last_ack" | "closed" | "delete_tcb"))
}

/// What kind of session this endpoint is, or None when the log leaves it out.
pub fn classify(e: &Endpoint) -> Option<Kind> {
    if is_loopback(&e.local_addr) {
        return None;
    }
    match (e.proto, e.state) {
        (Proto::Udp, _) | (Proto::Tcp, Some("listen")) => Some(Kind::Listener),
        (Proto::Tcp, state) => {
            let remote = e.remote_addr.as_deref()?;
            let unspecified = remote.parse::<IpAddr>().is_ok_and(|a| a.is_unspecified());
            (!is_loopback(remote) && !unspecified && !is_ending(state)).then_some(Kind::Connection)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    kind: Kind,
    proto: Proto,
    local_addr: String,
    local_port: u16,
    remote_addr: Option<String>,
    remote_port: Option<u16>,
    pid: u32,
}

struct Live {
    first: SystemTime,
    last: SystemTime,
    program: String,
    path: Option<String>,
    /// Short names of the services in this process, for the line and for telling services apart.
    services: Vec<String>,
    state: Option<&'static str>,
    new_peer: Option<bool>,
    /// Already there when logging was switched on, so `first_seen` is only a lower bound.
    already_open: bool,
}

/// The (program, remote address) pairs seen so far, oldest first, bounded.
#[derive(Default)]
pub struct Seen {
    order: VecDeque<String>,
    set: HashSet<String>,
    dirty: bool,
}

impl Seen {
    fn pair(program: &str, remote: &str) -> String {
        format!("{}|{}", program.to_lowercase(), remote)
    }

    /// True when the pair is new (and now remembered).
    pub fn insert(&mut self, program: &str, remote: &str) -> bool {
        let k = Self::pair(program, remote);
        if !self.set.insert(k.clone()) {
            return false;
        }
        self.order.push_back(k);
        while self.order.len() > SEEN_CAP {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        self.dirty = true;
        true
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn to_json(&self) -> Value {
        json!({"v": SCHEMA, "pairs": self.order})
    }

    /// Reads the file's text; anything unreadable gives an empty set rather than an error, since the
    /// worst outcome is that old pairs look new again.
    pub fn from_json(text: &str) -> Seen {
        let mut seen = Seen::default();
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            if let Some(list) = v.get("pairs").and_then(Value::as_array) {
                for p in list.iter().filter_map(Value::as_str) {
                    if seen.set.insert(p.to_string()) {
                        seen.order.push_back(p.to_string());
                    }
                }
            }
        }
        while seen.order.len() > SEEN_CAP {
            if let Some(old) = seen.order.pop_front() {
                seen.set.remove(&old);
            }
        }
        seen.dirty = false;
        seen
    }
}

/// Compares successive readings of the port tables and produces one line per session that ended.
#[derive(Default)]
pub struct Tracker {
    live: HashMap<Key, Live>,
    started: bool,
    pub seen: Seen,
}

fn secs_between(a: SystemTime, b: SystemTime) -> f64 {
    let ms = unix_secs(b).0 as i64 * 1000 + i64::from(unix_secs(b).1) - (unix_secs(a).0 as i64 * 1000 + i64::from(unix_secs(a).1));
    (ms.max(0) as f64 / 100.0).round() / 10.0
}

impl Tracker {
    pub fn with_seen(seen: Seen) -> Self {
        Tracker { seen, ..Default::default() }
    }

    pub fn open_sessions(&self) -> usize {
        self.live.len()
    }

    /// Takes one reading. Returns the lines for the sessions that were in the previous reading and
    /// are not in this one. The first reading only starts sessions (all marked `already_open`).
    pub fn observe(&mut self, snap: &PortsSnapshot, now: SystemTime) -> Vec<Value> {
        let first_reading = !self.started;
        self.started = true;
        let owners: HashMap<u32, &Owner> = snap.owners.iter().map(|o| (o.pid, o)).collect();

        let mut present: HashSet<Key> = HashSet::new();
        for e in &snap.endpoints {
            let Some(kind) = classify(e) else { continue };
            let key = Key {
                kind,
                proto: e.proto,
                local_addr: e.local_addr.clone(),
                local_port: e.local_port,
                remote_addr: e.remote_addr.clone(),
                remote_port: e.remote_port,
                pid: e.pid,
            };
            if !present.insert(key.clone()) {
                continue; // Windows can list the same socket twice
            }
            if let Some(l) = self.live.get_mut(&key) {
                l.last = now;
                l.state = e.state;
                continue;
            }
            let owner = owners.get(&e.pid);
            let program = owner.and_then(|o| o.name.clone()).unwrap_or_else(|| format!("unknown (pid {})", e.pid));
            let services: Vec<String> = owner.map(|o| o.services.iter().map(|s| s.name.clone()).collect()).unwrap_or_default();
            // Inside a shared host process the service tells peers apart: svchost.exe talking to an address as
            // Dnscache and as EventLog are different pairs.
            let who = peer_program(&program, &services);
            let new_peer = match (kind, &e.remote_addr) {
                (Kind::Connection, Some(r)) => Some(self.seen.insert(&who, r)),
                _ => None,
            };
            self.live.insert(
                key,
                Live { first: now, last: now, program, path: owner.and_then(|o| o.path.clone()), services, state: e.state, new_peer, already_open: first_reading },
            );
        }

        let gone: Vec<Key> = self.live.keys().filter(|k| !present.contains(*k)).cloned().collect();
        let mut lines: Vec<Value> = gone.into_iter().filter_map(|k| self.live.remove(&k).map(|l| line(&k, &l, "closed"))).collect();
        lines.sort_by(|a, b| a["first_seen"].as_str().cmp(&b["first_seen"].as_str()));
        lines
    }

    /// Ends every open session (logging switched off, or the app closing).
    pub fn finish(&mut self) -> Vec<Value> {
        let mut lines: Vec<Value> = self.live.drain().map(|(k, l)| line(&k, &l, "log_stopped")).collect();
        lines.sort_by(|a, b| a["first_seen"].as_str().cmp(&b["first_seen"].as_str()));
        self.started = false;
        lines
    }
}

/// The name a (program, address) pair is remembered under: the program, plus its services when it
/// hosts any (`svchost.exe[Dnscache]`).
fn peer_program(program: &str, services: &[String]) -> String {
    if services.is_empty() {
        program.to_string()
    } else {
        format!("{program}[{}]", services.join(","))
    }
}

fn line(k: &Key, l: &Live, ended_by: &str) -> Value {
    let mut v = json!({
        "v": SCHEMA,
        "event": match k.kind { Kind::Connection => "connection", Kind::Listener => "listener" },
        "first_seen": rfc3339_utc(l.first),
        "last_seen": rfc3339_utc(l.last),
        "duration_s": secs_between(l.first, l.last),
        "program": l.program,
        "path": l.path,
        "pid": k.pid,
        "proto": match k.proto { Proto::Tcp => "tcp", Proto::Udp => "udp" },
        "local_addr": k.local_addr,
        "local_port": k.local_port,
        "already_open": l.already_open,
        "ended_by": ended_by,
    });
    let obj = v.as_object_mut().expect("an object");
    if !l.services.is_empty() {
        obj.insert("services".into(), json!(l.services));
    }
    if k.kind == Kind::Connection {
        obj.insert("remote_addr".into(), json!(k.remote_addr));
        obj.insert("remote_port".into(), json!(k.remote_port));
        obj.insert("last_state".into(), json!(l.state));
        obj.insert("new_peer".into(), json!(l.new_peer));
    }
    v
}

/// The whole PC's traffic between two readings of the adapter counters, or None when there is nothing
/// trustworthy to say (no time between them, or no adapter that is up and in both readings). An adapter
/// whose counters went backwards (it was reset) is left out of that line rather than guessed at.
/// Only the adapters people mean by "my network" count, the same rule as the Network tab.
pub fn traffic_line(prev: &NetworkSnapshot, cur: &NetworkSnapshot, now: SystemTime, open_connections: usize) -> Option<Value> {
    let ms = cur.taken_ms.saturating_sub(prev.taken_ms);
    if ms == 0 {
        return None;
    }
    let seconds = ms as f64 / 1000.0;
    let (mut rx, mut tx) = (0u64, 0u64);
    let mut adapters = Vec::new();
    for a in cur.adapters.iter().filter(|a| a.primary && a.oper_status == 1) {
        let Some(p) = prev.adapters.iter().find(|p| p.luid == a.luid) else { continue };
        if a.rx_bytes < p.rx_bytes || a.tx_bytes < p.tx_bytes {
            continue;
        }
        let (r, t) = (a.rx_bytes - p.rx_bytes, a.tx_bytes - p.tx_bytes);
        rx += r;
        tx += t;
        adapters.push(json!({"name": a.name, "rx_bytes": r, "tx_bytes": t}));
    }
    if adapters.is_empty() {
        return None;
    }
    Some(json!({
        "v": SCHEMA,
        "event": "traffic",
        "t": rfc3339_utc(now),
        "seconds": seconds,
        "rx_bytes": rx,
        "tx_bytes": tx,
        "rx_bytes_per_s": (rx as f64 / seconds).round(),
        "tx_bytes_per_s": (tx as f64 / seconds).round(),
        "adapters": adapters,
        "open_connections": open_connections,
    }))
}

// ---------------------------------------------------------------------------------------------
// The log file.
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ConnLogStatus {
    pub enabled: bool,
    pub interval_s: u64,
    pub dir: String,
    pub file: Option<String>,
    /// Lines written since logging was switched on.
    pub lines: u64,
    /// Sessions being watched right now (not yet written).
    pub open_sessions: usize,
    /// How many (program, address) pairs are known, for the first-seen flag.
    pub seen_pairs: usize,
    pub error: Option<String>,
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
    tracker: Tracker,
    /// The adapter counters at the previous check, the other end of every traffic line.
    prev_net: Option<NetworkSnapshot>,
}

pub struct ConnLog {
    dir: PathBuf,
    app_version: String,
    state: Mutex<State>,
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
    for name in plan_prune_for(FILE_PREFIX, &files, today, KEEP_DAYS, CAP_BYTES) {
        let _ = fs::remove_file(dir.join(name));
    }
}

impl ConnLog {
    pub fn new(dir: PathBuf, app_version: &str) -> Self {
        ConnLog {
            dir,
            app_version: app_version.to_string(),
            state: Mutex::new(State {
                enabled: false,
                interval_s: DEFAULT_INTERVAL_S,
                out: None,
                due: None,
                lines: 0,
                error: None,
                tracker: Tracker::default(),
                prev_net: None,
            }),
        }
    }

    fn status_of(&self, st: &State) -> ConnLogStatus {
        ConnLogStatus {
            enabled: st.enabled,
            interval_s: st.interval_s,
            dir: self.dir.to_string_lossy().into_owned(),
            file: st.out.as_ref().map(|o| o.name.clone()),
            lines: st.lines,
            open_sessions: st.tracker.open_sessions(),
            seen_pairs: st.tracker.seen.len(),
            error: st.error.clone(),
            keep_days: KEEP_DAYS,
            cap_mb: CAP_BYTES / (1024 * 1024),
        }
    }

    pub fn status(&self) -> ConnLogStatus {
        self.status_of(&self.state.lock().unwrap())
    }

    fn load_seen(&self) -> Seen {
        fs::read_to_string(self.dir.join(SEEN_FILE)).map(|t| Seen::from_json(&t)).unwrap_or_default()
    }

    /// Writes the pair list through a temporary file, so a crash cannot leave half of it.
    fn save_seen(&self, st: &mut State) {
        if !st.tracker.seen.dirty {
            return;
        }
        let tmp = self.dir.join(format!("{SEEN_FILE}.tmp"));
        let result = fs::write(&tmp, st.tracker.seen.to_json().to_string()).and_then(|()| fs::rename(&tmp, self.dir.join(SEEN_FILE)));
        match result {
            Ok(()) => st.tracker.seen.dirty = false,
            Err(e) => st.error = Some(format!("could not save the list of known addresses: {e}")),
        }
    }

    pub fn set(&self, enabled: bool, interval_s: Option<u64>, now: SystemTime, at: Instant) -> ConnLogStatus {
        let mut st = self.state.lock().unwrap();
        if let Some(i) = interval_s {
            st.interval_s = i.clamp(MIN_INTERVAL_S, MAX_INTERVAL_S);
        }
        if enabled && !st.enabled {
            st.enabled = true;
            st.error = None;
            st.lines = 0;
            st.prev_net = None;
            if let Err(e) = fs::create_dir_all(&self.dir) {
                self.fail(&mut st, format!("could not create the log folder: {e}"));
            } else {
                prune_dir(&self.dir, day_number(now));
                st.tracker = Tracker::with_seen(self.load_seen());
                // Due at once: the first reading is what marks the sessions that were already open.
                st.due = Some(at);
                let line = json!({"v": SCHEMA, "event": "start", "t": rfc3339_utc(now), "app_version": self.app_version,
                    "interval_s": st.interval_s, "known_pairs": st.tracker.seen.len()});
                self.write(&mut st, &line, now);
            }
        } else if !enabled && st.enabled {
            self.stop(&mut st, now);
        } else if enabled {
            st.due = Some(at + Duration::from_secs(st.interval_s));
        }
        self.status_of(&st)
    }

    /// Writes the sessions still open, a `stop` line and saves the pair list.
    fn stop(&self, st: &mut State, now: SystemTime) {
        for l in st.tracker.finish() {
            self.write(st, &l, now);
        }
        let line = json!({"v": SCHEMA, "event": "stop", "t": rfc3339_utc(now)});
        self.write(st, &line, now);
        self.save_seen(st);
        st.enabled = false;
        st.out = None;
        st.due = None;
        st.prev_net = None;
    }

    fn fail(&self, st: &mut State, msg: String) {
        st.error = Some(msg);
        st.enabled = false;
        st.out = None;
        st.due = None;
    }

    fn write(&self, st: &mut State, line: &Value, now: SystemTime) {
        let day = day_number(now);
        if st.out.as_ref().is_none_or(|o| o.day != day) {
            let name = name_for_day(FILE_PREFIX, day);
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

    pub fn due(&self, at: Instant) -> bool {
        let st = self.state.lock().unwrap();
        st.enabled && st.due.is_some_and(|d| at >= d)
    }

    /// Takes one reading. `None` (the table could not be read) skips the round without closing
    /// anything, so a failed read is never mistaken for every connection ending at once.
    pub fn sample(&self, now: SystemTime, at: Instant, snap: Option<PortsSnapshot>, net: Option<NetworkSnapshot>) -> ConnLogStatus {
        let mut st = self.state.lock().unwrap();
        if !st.enabled {
            return self.status_of(&st);
        }
        st.due = Some(at + Duration::from_secs(st.interval_s));
        if let Some(snap) = snap {
            let ended = st.tracker.observe(&snap, now);
            // The traffic line comes first, then the sessions that ended during the same interval.
            if let Some(cur) = net {
                let line = st.prev_net.as_ref().and_then(|p| traffic_line(p, &cur, now, st.tracker.open_sessions()));
                if let Some(line) = line {
                    self.write(&mut st, &line, now);
                }
                st.prev_net = Some(cur);
            }
            for l in ended {
                self.write(&mut st, &l, now);
            }
            self.save_seen(&mut st);
        }
        self.status_of(&st)
    }
}

// ---------------------------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------------------------

#[tauri::command]
pub fn connlog_status(log: tauri::State<'_, ConnLog>) -> ConnLogStatus {
    log.status()
}

#[tauri::command]
pub fn connlog_set(app: tauri::AppHandle, log: tauri::State<'_, ConnLog>, enabled: bool, interval_s: Option<u64>) -> ConnLogStatus {
    use tauri::Emitter;
    let s = log.set(enabled, interval_s, SystemTime::now(), Instant::now());
    let _ = app.emit("connlog-status", s.clone());
    s
}

/// Called once a second by the sampler thread started in `run`.
pub fn tick(app: &tauri::AppHandle, log: &ConnLog) {
    use tauri::Emitter;
    let at = Instant::now();
    if !log.due(at) {
        return;
    }
    let s = log.sample(SystemTime::now(), at, crate::ports::snapshot().ok(), crate::network::snapshot().ok());
    let _ = app.emit("connlog-status", s);
}

/// Writes out the open sessions and stops; called when the window closes.
pub fn shutdown(log: &ConnLog) {
    log.set(false, None, SystemTime::now(), Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logger::day_of_name;
    use crate::network::{Adapter, Medium};
    use std::time::UNIX_EPOCH;

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn conn(local_port: u16, remote: &str, remote_port: u16, pid: u32, state: &'static str) -> Endpoint {
        Endpoint {
            proto: Proto::Tcp,
            v6: false,
            local_addr: "192.168.1.5".into(),
            local_port,
            remote_addr: Some(remote.into()),
            remote_port: Some(remote_port),
            state: Some(state),
            pid,
        }
    }

    fn listener(proto: Proto, addr: &str, port: u16, pid: u32) -> Endpoint {
        Endpoint {
            proto,
            v6: false,
            local_addr: addr.into(),
            local_port: port,
            remote_addr: None,
            remote_port: None,
            state: (proto == Proto::Tcp).then_some("listen"),
            pid,
        }
    }

    fn svc(name: &str) -> crate::ports::Service {
        crate::ports::Service { name: name.into(), display: name.into() }
    }

    fn snap(endpoints: Vec<Endpoint>) -> PortsSnapshot {
        PortsSnapshot {
            endpoints,
            owners: vec![
                Owner { pid: 10, name: Some("chrome.exe".into()), path: Some("C:\\chrome.exe".into()), services: vec![] },
                Owner { pid: 20, name: Some("Teams.exe".into()), path: None, services: vec![] },
                Owner { pid: 30, name: Some("svchost.exe".into()), path: None, services: vec![svc("Dnscache")] },
                Owner { pid: 31, name: Some("svchost.exe".into()), path: None, services: vec![svc("EventLog"), svc("Schedule")] },
            ],
            services_error: None,
        }
    }

    fn adapter(luid: u64, name: &str, rx: u64, tx: u64) -> Adapter {
        Adapter {
            luid,
            name: name.into(),
            description: String::new(),
            medium: Medium::Ethernet,
            hardware: true,
            primary: true,
            oper_status: 1,
            media_connected: Some(true),
            rx_speed_bps: None,
            tx_speed_bps: None,
            rx_bytes: rx,
            tx_bytes: tx,
            rx_packets: 0,
            tx_packets: 0,
            rx_errors: 0,
            tx_errors: 0,
            rx_discards: 0,
            tx_discards: 0,
        }
    }

    fn net(taken_ms: u64, adapters: Vec<Adapter>) -> NetworkSnapshot {
        NetworkSnapshot { taken_ms, adapters }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("proclens-connlog-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn loopback_closing_and_local_only_listeners_are_left_out() {
        assert_eq!(classify(&conn(50000, "140.82.112.3", 443, 10, "established")), Some(Kind::Connection));
        assert_eq!(classify(&conn(50000, "140.82.112.3", 443, 10, "close_wait")), Some(Kind::Connection));
        assert_eq!(classify(&conn(50000, "140.82.112.3", 443, 10, "time_wait")), None, "already closing");
        assert_eq!(classify(&conn(50000, "127.0.0.1", 5173, 10, "established")), None, "loopback peer");
        assert_eq!(classify(&conn(50000, "::1", 5173, 10, "established")), None, "IPv6 loopback peer");
        let mut from_loopback = conn(50000, "10.0.0.2", 80, 10, "established");
        from_loopback.local_addr = "127.0.0.1".into();
        assert_eq!(classify(&from_loopback), None);
        assert_eq!(classify(&listener(Proto::Tcp, "0.0.0.0", 80, 10)), Some(Kind::Listener));
        assert_eq!(classify(&listener(Proto::Udp, "192.168.1.5", 5353, 10)), Some(Kind::Listener));
        assert_eq!(classify(&listener(Proto::Tcp, "127.0.0.1", 5173, 10)), None, "listener that only accepts local connections");
        assert_eq!(classify(&listener(Proto::Udp, "::1", 1900, 10)), None);
    }

    #[test]
    fn a_session_is_written_once_when_it_ends_with_when_it_was_first_and_last_seen() {
        let mut tr = Tracker::default();
        let c = conn(50000, "140.82.112.3", 443, 10, "established");
        assert!(tr.observe(&snap(vec![c.clone()]), t(100)).is_empty(), "nothing is written while it is open");
        assert!(tr.observe(&snap(vec![c.clone()]), t(110)).is_empty());
        assert_eq!(tr.open_sessions(), 1);
        let lines = tr.observe(&snap(vec![]), t(120));
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(l["event"], "connection");
        assert_eq!(l["program"], "chrome.exe");
        assert_eq!(l["path"], "C:\\chrome.exe");
        assert_eq!(l["remote_addr"], "140.82.112.3");
        assert_eq!(l["remote_port"], 443);
        assert_eq!(l["first_seen"], rfc3339_utc(t(100)));
        assert_eq!(l["last_seen"], rfc3339_utc(t(110)), "the last reading that still had it, not the one that missed it");
        assert_eq!(l["duration_s"], 10.0);
        assert_eq!(l["ended_by"], "closed");
        assert_eq!(tr.open_sessions(), 0);
        assert!(tr.observe(&snap(vec![]), t(130)).is_empty(), "and never again");
    }

    #[test]
    fn what_was_already_open_when_logging_started_is_marked_so() {
        let mut tr = Tracker::default();
        let c = conn(50000, "140.82.112.3", 443, 10, "established");
        tr.observe(&snap(vec![c.clone()]), t(100));
        tr.observe(&snap(vec![c.clone(), conn(50001, "1.1.1.1", 443, 10, "established")]), t(110));
        let lines = tr.observe(&snap(vec![]), t(120));
        let by_port = |p: u64| lines.iter().find(|l| l["local_port"] == p).unwrap();
        assert_eq!(by_port(50000)["already_open"], true);
        assert_eq!(by_port(50001)["already_open"], false);
    }

    #[test]
    fn a_connection_that_starts_to_close_ends_the_session_at_once() {
        let mut tr = Tracker::default();
        tr.observe(&snap(vec![conn(50000, "140.82.112.3", 443, 10, "established")]), t(100));
        let lines = tr.observe(&snap(vec![conn(50000, "140.82.112.3", 443, 0, "time_wait")]), t(110));
        assert_eq!(lines.len(), 1, "the TIME_WAIT row (pid 0) is a different key and is not tracked");
        assert_eq!(lines[0]["last_state"], "established");
    }

    #[test]
    fn new_peer_is_true_once_per_program_and_address() {
        let mut tr = Tracker::default();
        let a = conn(50000, "140.82.112.3", 443, 10, "established");
        let b = conn(50001, "140.82.112.3", 443, 10, "established"); // same program, same address
        let c = conn(50002, "140.82.112.3", 443, 20, "established"); // another program, same address
        tr.observe(&snap(vec![a, b, c]), t(100));
        let lines = tr.finish();
        let flags: Vec<(String, bool)> =
            lines.iter().map(|l| (l["program"].as_str().unwrap().to_string(), l["new_peer"].as_bool().unwrap())).collect();
        assert_eq!(flags.iter().filter(|(p, n)| p == "chrome.exe" && *n).count(), 1, "{flags:?}");
        assert_eq!(flags.iter().filter(|(p, n)| p == "chrome.exe" && !*n).count(), 1, "{flags:?}");
        assert_eq!(flags.iter().filter(|(p, n)| p == "Teams.exe" && *n).count(), 1);
        assert_eq!(tr.seen.len(), 2);
    }

    #[test]
    fn the_same_socket_listed_twice_is_one_session() {
        let mut tr = Tracker::default();
        let l = listener(Proto::Udp, "0.0.0.0", 3702, 10);
        tr.observe(&snap(vec![l.clone(), l]), t(100));
        assert_eq!(tr.open_sessions(), 1);
    }

    #[test]
    fn listeners_have_no_remote_fields_and_no_new_peer() {
        let mut tr = Tracker::default();
        tr.observe(&snap(vec![listener(Proto::Tcp, "0.0.0.0", 8080, 10)]), t(100));
        let lines = tr.observe(&snap(vec![]), t(110));
        let l = &lines[0];
        assert_eq!(l["event"], "listener");
        assert_eq!(l["local_port"], 8080);
        assert_eq!(l["proto"], "tcp");
        assert!(l.get("remote_addr").is_none() && l.get("new_peer").is_none());
    }

    #[test]
    fn an_unknown_program_is_named_by_its_pid() {
        let mut tr = Tracker::default();
        tr.observe(&snap(vec![conn(50000, "1.2.3.4", 80, 999, "established")]), t(100));
        assert_eq!(tr.finish()[0]["program"], "unknown (pid 999)");
    }

    #[test]
    fn finish_writes_every_open_session_marked_log_stopped() {
        let mut tr = Tracker::default();
        tr.observe(&snap(vec![conn(50000, "1.2.3.4", 80, 10, "established"), listener(Proto::Tcp, "0.0.0.0", 80, 10)]), t(100));
        let lines = tr.finish();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l["ended_by"] == "log_stopped"));
        assert_eq!(tr.open_sessions(), 0);
    }

    #[test]
    fn the_known_pairs_survive_a_round_trip_and_are_bounded() {
        let mut s = Seen::default();
        assert!(s.insert("Chrome.exe", "1.2.3.4"));
        assert!(!s.insert("chrome.exe", "1.2.3.4"), "program names compare ignoring case");
        let back = Seen::from_json(&s.to_json().to_string());
        assert_eq!(back.len(), 1);
        assert!(!{ let mut b = back; b.insert("chrome.exe", "1.2.3.4") });
        assert_eq!(Seen::from_json("not json").len(), 0, "unreadable means empty, not an error");

        let mut big = Seen::default();
        for i in 0..SEEN_CAP + 5 {
            big.insert("p", &i.to_string());
        }
        assert_eq!(big.len(), SEEN_CAP);
        assert!(big.insert("p", "0"), "the oldest was forgotten, so it is new again");
    }

    #[test]
    fn the_two_logs_never_prune_each_others_files() {
        let day = 20_000;
        let activity = crate::logger::file_name_for_day(day);
        let conns = name_for_day(FILE_PREFIX, day);
        assert_ne!(activity, conns);
        assert_eq!(day_of_name(FILE_PREFIX, &activity), None);
        assert_eq!(crate::logger::day_of_file_name(&conns), None);
        assert_eq!(day_of_name(FILE_PREFIX, &conns), Some(day));
        assert_eq!(day_of_name(FILE_PREFIX, SEEN_FILE), None, "the pair list is never pruned");
        let files = vec![(activity.clone(), 5), (conns.clone(), 5), (SEEN_FILE.to_string(), 5)];
        assert_eq!(plan_prune_for(FILE_PREFIX, &files, day + 100, 28, u64::MAX), vec![conns]);
        assert_eq!(crate::logger::plan_prune(&files, day + 100, 28, u64::MAX), vec![activity]);
    }

    #[test]
    fn switching_on_writes_a_start_line_and_off_writes_open_sessions_and_a_stop_line() {
        let dir = tmp("onoff");
        let log = ConnLog::new(dir.clone(), "9.9.9");
        let at = Instant::now();
        let s = log.set(true, Some(5), t(1_800_000_000), at);
        assert!(s.enabled && s.error.is_none(), "{s:?}");
        assert!(log.due(at), "the first reading is due at once");

        let c = conn(50000, "140.82.112.3", 443, 10, "established");
        log.sample(t(1_800_000_001), at, Some(snap(vec![c.clone()])), None);
        assert!(!log.due(at), "and the next one waits for the interval");
        assert_eq!(log.status().open_sessions, 1);
        // A failed read neither closes nor loses anything.
        log.sample(t(1_800_000_006), at, None, None);
        assert_eq!(log.status().open_sessions, 1);

        let s = log.set(false, None, t(1_800_000_010), at);
        assert!(!s.enabled);
        let name = name_for_day(FILE_PREFIX, day_number(t(1_800_000_000)));
        let text = fs::read_to_string(dir.join(&name)).unwrap();
        let events: Vec<String> =
            text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["event"].as_str().unwrap().to_string()).collect();
        assert_eq!(events, vec!["start", "connection", "stop"]);
        assert!(text.contains("140.82.112.3"), "this log is the one that holds addresses");
        assert!(dir.join(SEEN_FILE).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_second_run_knows_the_pairs_the_first_one_saw() {
        let dir = tmp("seen");
        let log = ConnLog::new(dir.clone(), "9.9.9");
        let at = Instant::now();
        log.set(true, None, t(1_800_000_000), at);
        log.sample(t(1_800_000_001), at, Some(snap(vec![conn(50000, "140.82.112.3", 443, 10, "established")])), None);
        log.set(false, None, t(1_800_000_010), at);

        let log2 = ConnLog::new(dir.clone(), "9.9.9");
        let s = log2.set(true, None, t(1_800_000_100), at);
        assert_eq!(s.seen_pairs, 1);
        log2.sample(t(1_800_000_101), at, Some(snap(vec![conn(50009, "140.82.112.3", 443, 10, "established")])), None);
        log2.set(false, None, t(1_800_000_110), at);
        let name = name_for_day(FILE_PREFIX, day_number(t(1_800_000_000)));
        let text = fs::read_to_string(dir.join(&name)).unwrap();
        let last_conn: Value = serde_json::from_str(text.lines().rfind(|l| l.contains("\"connection\"")).unwrap()).unwrap();
        assert_eq!(last_conn["new_peer"], false, "seen in the earlier run");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_interval_is_clamped() {
        let dir = tmp("interval");
        let log = ConnLog::new(dir.clone(), "9.9.9");
        assert_eq!(log.set(false, Some(1), t(1), Instant::now()).interval_s, MIN_INTERVAL_S);
        assert_eq!(log.set(false, Some(9999), t(1), Instant::now()).interval_s, MAX_INTERVAL_S);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unwritable_folder_is_reported_not_retried() {
        let dir = tmp("blocked");
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        fs::write(&dir, "a file where the folder should be").unwrap();
        let log = ConnLog::new(dir.clone(), "9.9.9");
        let s = log.set(true, None, t(1_800_000_000), Instant::now());
        assert!(!s.enabled && s.error.is_some(), "{s:?}");
        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn traffic_is_bytes_and_rates_between_two_readings_per_adapter() {
        let prev = net(1_000, vec![adapter(1, "Ethernet", 1_000, 500), adapter(2, "USB", 0, 0)]);
        let cur = net(3_000, vec![adapter(1, "Ethernet", 5_000, 700), adapter(2, "USB", 100, 0)]); // 2 s apart
        let l = traffic_line(&prev, &cur, t(100), 7).unwrap();
        assert_eq!(l["event"], "traffic");
        assert_eq!(l["seconds"], 2.0);
        assert_eq!((l["rx_bytes"].as_u64(), l["tx_bytes"].as_u64()), (Some(4_100), Some(200)));
        assert_eq!((l["rx_bytes_per_s"].as_f64(), l["tx_bytes_per_s"].as_f64()), (Some(2_050.0), Some(100.0)));
        assert_eq!(l["open_connections"], 7);
        assert_eq!(l["adapters"][0], json!({"name": "Ethernet", "rx_bytes": 4_000, "tx_bytes": 200}));
        assert_eq!(l["adapters"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn traffic_leaves_out_adapters_that_are_down_not_primary_reset_or_new() {
        let mut down = adapter(2, "Down", 0, 0);
        down.oper_status = 2;
        let mut soft = adapter(3, "WAN Miniport", 0, 0);
        soft.primary = false;
        let prev = net(0, vec![adapter(1, "Ethernet", 100, 100), down.clone(), soft.clone(), adapter(4, "Reset", 900, 900)]);
        let mut down_later = down;
        down_later.rx_bytes = 50;
        let mut soft_later = soft;
        soft_later.rx_bytes = 50;
        let cur = net(1_000, vec![adapter(1, "Ethernet", 200, 150), down_later, soft_later, adapter(4, "Reset", 10, 10), adapter(5, "New", 77, 77)]);
        let l = traffic_line(&prev, &cur, t(1), 0).unwrap();
        let names: Vec<&str> = l["adapters"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Ethernet"], "down, software, counter-reset and just-appeared adapters are skipped");
        assert_eq!(l["rx_bytes"], 100);
    }

    #[test]
    fn no_traffic_line_without_time_between_readings_or_a_usable_adapter() {
        let a = net(5_000, vec![adapter(1, "Ethernet", 1, 1)]);
        assert!(traffic_line(&a, &a, t(1), 0).is_none(), "no time between them");
        assert!(traffic_line(&net(0, vec![]), &net(1_000, vec![adapter(1, "Ethernet", 1, 1)]), t(1), 0).is_none(), "no adapter in both");
        let prev = net(0, vec![adapter(1, "Ethernet", 10, 10)]);
        let went_back = net(1_000, vec![adapter(1, "Ethernet", 5, 5)]);
        assert!(traffic_line(&prev, &went_back, t(1), 0).is_none(), "a reset is not reported as traffic");
    }

    #[test]
    fn each_check_writes_a_traffic_line_before_the_sessions_that_ended_but_never_for_the_first_reading() {
        let dir = tmp("traffic");
        let log = ConnLog::new(dir.clone(), "9.9.9");
        let at = Instant::now();
        log.set(true, Some(5), t(1_800_000_000), at);
        let c = conn(50000, "140.82.112.3", 443, 10, "established");
        // First check: nothing to compare with yet.
        log.sample(t(1_800_000_001), at, Some(snap(vec![c.clone()])), Some(net(1_000, vec![adapter(1, "Ethernet", 0, 0)])));
        // Second: the connection is gone and the counters moved.
        log.sample(t(1_800_000_006), at, Some(snap(vec![])), Some(net(6_000, vec![adapter(1, "Ethernet", 5_000_000, 100_000)])));
        // Third: the counters could not be read; nothing is invented and nothing closes.
        log.sample(t(1_800_000_011), at, Some(snap(vec![])), None);
        // Fourth: the interval is measured from the last good reading, so it is 10 s, not 5.
        log.sample(t(1_800_000_016), at, Some(snap(vec![])), Some(net(16_000, vec![adapter(1, "Ethernet", 5_000_000 + 10_000_000, 100_000)])));
        log.set(false, None, t(1_800_000_020), at);

        let name = name_for_day(FILE_PREFIX, day_number(t(1_800_000_000)));
        let lines: Vec<Value> = fs::read_to_string(dir.join(&name)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let events: Vec<&str> = lines.iter().map(|l| l["event"].as_str().unwrap()).collect();
        assert_eq!(events, vec!["start", "traffic", "connection", "traffic", "stop"], "{events:?}");
        assert_eq!(lines[1]["seconds"], 5.0);
        assert_eq!(lines[1]["rx_bytes_per_s"], 1_000_000.0);
        assert_eq!(lines[1]["open_connections"], 0);
        assert_eq!(lines[3]["seconds"], 10.0);
        assert_eq!(lines[3]["rx_bytes_per_s"], 1_000_000.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shared_host_process_is_told_apart_by_its_services() {
        let mut tr = Tracker::default();
        // The same program, the same remote address, but two different svchost processes.
        tr.observe(
            &snap(vec![
                conn(50000, "20.190.159.23", 443, 30, "established"),
                conn(50001, "20.190.159.23", 443, 31, "established"),
                conn(50002, "20.190.159.23", 443, 31, "established"),
            ]),
            t(100),
        );
        let lines = tr.finish();
        let by = |port: u64| lines.iter().find(|l| l["local_port"] == port).unwrap();
        assert_eq!(by(50000)["services"], json!(["Dnscache"]));
        assert_eq!(by(50001)["services"], json!(["EventLog", "Schedule"]));
        assert_eq!(by(50000)["new_peer"], true);
        let flags = [by(50001)["new_peer"].as_bool().unwrap(), by(50002)["new_peer"].as_bool().unwrap()];
        assert_eq!(flags.iter().filter(|f| **f).count(), 1, "the second service host is new once, not twice: {flags:?}");
        assert_eq!(tr.seen.len(), 2, "svchost.exe[Dnscache] and svchost.exe[EventLog,Schedule]");
    }

    #[test]
    fn an_ordinary_program_has_no_services_field() {
        let mut tr = Tracker::default();
        tr.observe(&snap(vec![conn(50000, "1.2.3.4", 80, 10, "established")]), t(100));
        assert!(tr.finish()[0].get("services").is_none());
    }
}
