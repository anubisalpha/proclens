//! A kernel ETW trace session that accumulates activity and hands it out one interval at a time.
//!
//! Everything about event layout here is specific to a Windows build and was verified on the build
//! recorded in `profiles/` (see `windows-10-19045.json` for the opcode tables and quirks).
//!
//! State is split in two: *persistent* state that must survive between intervals (thread-to-pid
//! map, file names, in-flight I/O requests) and the *interval* aggregates, which
//! [`TraceSession::take_interval`] swaps out so each snapshot is a delta.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ferrisetw::parser::{Parser, Pointer};
use ferrisetw::provider::kernel_providers::{self, KernelProvider};
use ferrisetw::provider::Provider;
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::{KernelTrace, TraceProperties};
use ferrisetw::EventRecord;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Diagnostics::Etw::{
    ControlTraceW, CONTROLTRACE_HANDLE, EVENT_TRACE_CONTROL_QUERY, EVENT_TRACE_PROPERTIES,
};
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::Win32::System::ProcessStatus::{EnumDeviceDrivers, GetDeviceDriverBaseNameW};

use crate::files;

/// Bytes with no identifiable file are reported under this name.
pub const UNRESOLVED: &str = "(file not identified: opened before the trace, handle gone or unreadable)";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Thread,
    FileIo,
    DiskIo,
    PerfInfo,
}

#[derive(Default, Debug)]
pub struct IoAgg {
    pub reads: u64,
    pub read_bytes: u64,
    pub writes: u64,
    pub write_bytes: u64,
    /// Sum of disk response times (raw ticks) and how many requests contributed.
    pub resp_ticks: u64,
    pub resp_count: u64,
}

#[derive(Default, Debug)]
pub struct TimeAgg {
    pub count: u64,
    /// Sum of (event QPC time - InitialTime) in QPC ticks. Needs the vendored ferrisetw patch.
    pub sum_ticks: i128,
    pub max_ticks: i64,
}

/// Everything observed since the previous [`TraceSession::take_interval`].
#[derive(Default, Debug)]
pub struct Interval {
    pub elapsed: Duration,
    /// Physical disk requests, by issuing process.
    pub disk_by_pid: HashMap<u32, IoAgg>,
    /// File-level reads/writes (bytes actually transferred), by requesting process.
    pub file_by_pid: HashMap<u32, IoAgg>,
    pub file_bytes: HashMap<String, u64>,
    pub dpc_by_routine: HashMap<usize, TimeAgg>,
    pub isr_by_routine: HashMap<usize, TimeAgg>,
    pub requested_total: u64,
    pub actual_total: u64,
    /// File requests that failed for a reason other than reaching end-of-file.
    pub io_failed: u64,
    /// Bytes moved by paging I/O (cache flushes, page-ins). Kept out of the per-process and
    /// per-file figures, which describe what programs asked for; it shows up in disk activity.
    pub paging_bytes: u64,
    pub io_unmatched_end: u64,
    pub dur_negative: u64,
    pub dur_implausible: u64,
    pub disk_attributed: u64,
    pub disk_fallback: u64,
}

struct PendingIo {
    pid: u32,
    obj: usize,
    read: bool,
    requested: u64,
    /// IRP_PAGING_IO / IRP_SYNCHRONOUS_PAGING_IO: the cache manager or memory manager moving
    /// pages (flushes, page-ins), as opposed to the program's own read/write calls.
    paging: bool,
}

/// NTSTATUS for "read past the end of the file". Normal at the end of every sequential read.
const STATUS_END_OF_FILE: u32 = 0xC000_0011;
const IRP_PAGING_IO: u32 = 0x0000_0002;
const IRP_SYNCHRONOUS_PAGING_IO: u32 = 0x0000_0040;

#[derive(Default)]
struct State {
    // --- persistent across intervals ---
    tid_to_pid: HashMap<u32, u32>,
    file_names: HashMap<usize, String>,
    pending_io: HashMap<usize, PendingIo>,
    /// Actual bytes for FileObjects with no name yet; merged when named or closed.
    unnamed_bytes: HashMap<usize, u64>,
    /// FileObjects already looked up in the handle table (each is tried once).
    resolve_tried: HashSet<usize>,
    max_plausible_ticks: i64,
    // --- cumulative diagnostics ---
    seen: HashMap<(Kind, u8), (String, u64)>,
    parse_errors: HashMap<String, u64>,
    schema_errors: u64,
    names_from_create: u64,
    names_from_handles: u64,
    // --- current interval ---
    cur: Interval,
}

impl State {
    fn err(&mut self, kind: Kind, opcode: u8, field: &str) {
        *self.parse_errors.entry(format!("{kind:?}/op{opcode}/{field}")).or_default() += 1;
    }

    /// Moves bytes held for an unnamed FileObject into the by-name table.
    fn flush_obj(&mut self, obj: usize) {
        if let Some(b) = self.unnamed_bytes.remove(&obj) {
            let key = self.file_names.get(&obj).cloned().unwrap_or_else(|| UNRESOLVED.to_string());
            *self.cur.file_bytes.entry(key).or_default() += b;
        }
    }
}

fn handler(
    state: Arc<Mutex<State>>,
    kind: Kind,
) -> impl FnMut(&EventRecord, &SchemaLocator) + Send + Sync + 'static {
    move |record, locator| {
        let opcode = record.opcode();
        let wanted = match kind {
            Kind::Thread => matches!(opcode, 1 | 3),
            Kind::FileIo => matches!(opcode, 0 | 32 | 35 | 36 | 64 | 66 | 67 | 68 | 76),
            Kind::DiskIo => matches!(opcode, 10 | 11),
            Kind::PerfInfo => matches!(opcode, 50 | 66..=69),
        };
        // Schema lookups (TDH) are costly at ~100k events/s: only do them for events we parse,
        // plus once per (kind, opcode) to learn its name for diagnostics.
        let first_seen = {
            let mut s = state.lock().unwrap();
            let e = s.seen.entry((kind, opcode)).or_default();
            e.1 += 1;
            e.1 == 1
        };
        if !wanted && !first_seen {
            return;
        }
        let schema = match locator.event_schema(record) {
            Ok(s) => s,
            Err(_) => {
                state.lock().unwrap().schema_errors += 1;
                return;
            }
        };
        if first_seen {
            if let Some(e) = state.lock().unwrap().seen.get_mut(&(kind, opcode)) {
                e.0 = schema.opcode_name();
            }
        }
        if !wanted {
            return;
        }
        let parser = Parser::create(record, &schema);
        let mut s = state.lock().unwrap();

        macro_rules! field {
            ($t:ty, $name:expr) => {
                match parser.try_parse::<$t>($name) {
                    Ok(v) => Some(v),
                    Err(_) => {
                        s.err(kind, opcode, $name);
                        None
                    }
                }
            };
        }

        match kind {
            Kind::Thread => {
                // 1 = Start, 3 = DCStart (rundown of threads that already existed)
                if let (Some(pid), Some(tid)) = (field!(u32, "ProcessId"), field!(u32, "TThreadId")) {
                    s.tid_to_pid.insert(tid, pid);
                }
            }
            Kind::FileIo => match opcode {
                0 | 32 | 35 | 36 => {
                    // Name events: never observed in a live session on the verified build, but
                    // harmless if another build sends them.
                    if let (Some(obj), Some(name)) = (field!(Pointer, "FileObject"), field!(String, "FileName")) {
                        s.file_names.insert(*obj, name);
                    }
                }
                // Close: the kernel recycles FileObject addresses, so settle and forget the object.
                66 => {
                    if let Some(obj) = field!(Pointer, "FileObject") {
                        s.flush_obj(*obj);
                        s.file_names.remove(&*obj);
                        s.resolve_tried.remove(&*obj);
                    }
                }
                64 => {
                    if let (Some(obj), Some(name)) = (field!(Pointer, "FileObject"), field!(String, "OpenPath")) {
                        s.flush_obj(*obj); // leftovers belong to a previous incarnation of this address
                        s.resolve_tried.remove(&*obj);
                        s.file_names.insert(*obj, name);
                        s.names_from_create += 1;
                    }
                }
                // Read/Write request: remember it. IoSize is only what the caller *asked* for.
                67 | 68 => {
                    let requested = field!(u32, "IoSize").unwrap_or(0) as u64;
                    let io_flags = field!(u32, "IoFlags").unwrap_or(0);
                    if let (Some(irp), Some(obj)) = (field!(Pointer, "IrpPtr"), field!(Pointer, "FileObject")) {
                        if s.pending_io.len() > 200_000 {
                            s.pending_io.clear(); // completions lost or never coming
                        }
                        s.pending_io.insert(
                            *irp,
                            PendingIo {
                                pid: record.process_id(),
                                obj: *obj,
                                read: opcode == 67,
                                requested,
                                paging: io_flags & (IRP_PAGING_IO | IRP_SYNCHRONOUS_PAGING_IO) != 0,
                            },
                        );
                    }
                }
                // OperationEnd: ExtraInfo is the number of bytes actually transferred.
                76 => {
                    if let Some(irp) = field!(Pointer, "IrpPtr") {
                        match s.pending_io.remove(&*irp) {
                            None => s.cur.io_unmatched_end += 1,
                            Some(p) => {
                                let status = field!(u32, "NtStatus").unwrap_or(0);
                                let actual = if status == 0 {
                                    field!(Pointer, "ExtraInfo").map(|v| *v as u64).unwrap_or(0)
                                } else {
                                    if status != STATUS_END_OF_FILE {
                                        s.cur.io_failed += 1;
                                    }
                                    0
                                };
                                if p.paging {
                                    s.cur.paging_bytes += actual;
                                } else {
                                    s.cur.requested_total += p.requested;
                                    s.cur.actual_total += actual;
                                    let agg = s.cur.file_by_pid.entry(p.pid).or_default();
                                    if p.read {
                                        agg.reads += 1;
                                        agg.read_bytes += actual;
                                    } else {
                                        agg.writes += 1;
                                        agg.write_bytes += actual;
                                    }
                                    match s.file_names.get(&p.obj).cloned() {
                                        Some(name) => *s.cur.file_bytes.entry(name).or_default() += actual,
                                        None => *s.unnamed_bytes.entry(p.obj).or_default() += actual,
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            Kind::DiskIo => {
                let size = field!(u32, "TransferSize").unwrap_or(0) as u64;
                let resp = field!(u64, "HighResResponseTime").unwrap_or(0);
                let issuing = field!(u32, "IssuingThreadId");
                let pid = match issuing.and_then(|t| s.tid_to_pid.get(&t).copied()) {
                    Some(p) => {
                        s.cur.disk_attributed += 1;
                        p
                    }
                    None => {
                        s.cur.disk_fallback += 1;
                        record.process_id()
                    }
                };
                let agg = s.cur.disk_by_pid.entry(pid).or_default();
                agg.resp_ticks += resp;
                agg.resp_count += 1;
                if opcode == 10 {
                    agg.reads += 1;
                    agg.read_bytes += size;
                } else {
                    agg.writes += 1;
                    agg.write_bytes += size;
                }
            }
            Kind::PerfInfo => {
                // 50 = ISR-MSI and 67 = ISR; 66 ThreadedDPC, 68 DPC, 69 TimerDPC
                let init = field!(u64, "InitialTime");
                let routine = field!(Pointer, "Routine");
                if let (Some(init), Some(routine)) = (init, routine) {
                    // With the vendored ferrisetw patch the header timestamp is QPC, the same
                    // clock as InitialTime; the event is logged at the end of the routine.
                    let d = record.raw_timestamp() - init as i64;
                    let st: &mut State = &mut s;
                    if d < 0 {
                        st.cur.dur_negative += 1;
                    } else if d > st.max_plausible_ticks {
                        // e.g. InitialTime = 0 yields "time since boot"
                        st.cur.dur_implausible += 1;
                    } else {
                        let map = if matches!(opcode, 50 | 67) {
                            &mut st.cur.isr_by_routine
                        } else {
                            &mut st.cur.dpc_by_routine
                        };
                        let a = map.entry(*routine).or_default();
                        a.count += 1;
                        a.sum_ticks += d as i128;
                        a.max_ticks = a.max_ticks.max(d);
                    }
                }
            }
        }
    }
}

/// One pass of "which file is this FileObject?" for objects that have bytes but no name yet.
/// Each object is tried once; handles can be gone or unreadable, in which case it stays unnamed.
fn resolve_pass(state: &Mutex<State>) {
    let wanted: HashSet<usize> = {
        let mut s = state.lock().unwrap();
        let candidates: Vec<usize> = s
            .unnamed_bytes
            .keys()
            .filter(|o| !s.file_names.contains_key(*o) && !s.resolve_tried.contains(*o))
            .copied()
            .collect();
        s.resolve_tried.extend(candidates.iter().copied());
        candidates.into_iter().collect()
    };
    if wanted.is_empty() {
        return;
    }
    let names = files::resolve(&wanted);
    let mut s = state.lock().unwrap();
    for (obj, name) in names {
        // Only apply if the object is still live and unnamed (it may have closed meanwhile).
        if s.unnamed_bytes.contains_key(&obj) && !s.file_names.contains_key(&obj) {
            s.file_names.insert(obj, name);
            s.names_from_handles += 1;
            s.flush_obj(obj);
        }
    }
}

pub struct SessionHealth {
    pub events_lost: u32,
    pub log_buffers_lost: u32,
    pub rt_buffers_lost: u32,
    pub buffers_written: u32,
    pub buffers: u32,
    pub free_buffers: u32,
}

/// Reads the live session's counters from the kernel; lost events are otherwise invisible.
fn query_session(name: &str) -> Option<SessionHealth> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let header = std::mem::size_of::<EVENT_TRACE_PROPERTIES>();
    // EVENT_TRACE_PROPERTIES is followed by the logger name and log file name buffers.
    let total = header + 2 * 1024;
    let mut buf = vec![0u8; total];
    let props = buf.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
    unsafe {
        (*props).Wnode.BufferSize = total as u32;
        (*props).LoggerNameOffset = header as u32;
        (*props).LogFileNameOffset = (header + 1024) as u32;
        let rc = ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            PCWSTR(wide.as_ptr()),
            props,
            EVENT_TRACE_CONTROL_QUERY,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        Some(SessionHealth {
            events_lost: (*props).EventsLost,
            log_buffers_lost: (*props).LogBuffersLost,
            rt_buffers_lost: (*props).RealTimeBuffersLost,
            buffers_written: (*props).BuffersWritten,
            buffers: (*props).NumberOfBuffers,
            free_buffers: (*props).FreeBuffers,
        })
    }
}

/// What the session has seen, for the CLI report and for debugging a new Windows build.
pub struct Diagnostics {
    /// (kind, opcode, opcode name, count)
    pub seen: Vec<(String, u8, String, u64)>,
    pub parse_errors: Vec<(String, u64)>,
    pub schema_errors: u64,
    pub names_from_create: u64,
    pub names_from_handles: u64,
}

pub struct TraceSession {
    name: String,
    state: Arc<Mutex<State>>,
    trace: Option<KernelTrace>,
    stop: Arc<AtomicBool>,
    resolver: Option<JoinHandle<()>>,
    last_take: Mutex<Instant>,
    pub qpc_freq: i64,
}

impl TraceSession {
    /// Starts a kernel session called `name` (stopping a stale one of the same name first).
    /// Needs an elevated process.
    pub fn start(name: &str) -> Result<Self, String> {
        let mut qpc_freq = 0i64;
        let _ = unsafe { QueryPerformanceFrequency(&mut qpc_freq) };
        let qpc_freq = qpc_freq.max(1);

        let state = Arc::new(Mutex::new(State::default()));
        state.lock().unwrap().max_plausible_ticks = qpc_freq; // one second

        // Needed to duplicate handles out of other processes for file-name resolution.
        if !matches!(crate::mem::enable_privilege(w!("SeDebugPrivilege")), Ok(true)) {
            eprintln!("warning: SeDebugPrivilege not available; file-name resolution will be limited");
        }

        let thread = Provider::kernel(&kernel_providers::THREAD_PROVIDER)
            .add_callback(handler(state.clone(), Kind::Thread))
            .build();
        // File events come from three flags: FILE_IO (completions, op76), FILE_IO_INIT (the
        // read/write requests) and DISK_FILE_IO; one provider on the FileIo GUID, flags ORed.
        let file_def = KernelProvider::new(
            kernel_providers::FILE_IO_PROVIDER.guid,
            0x0200_0000 | 0x0400_0000 | 0x0000_0200,
        );
        let file = Provider::kernel(&file_def).add_callback(handler(state.clone(), Kind::FileIo)).build();
        let disk = Provider::kernel(&kernel_providers::DISK_IO_PROVIDER)
            .add_callback(handler(state.clone(), Kind::DiskIo))
            .build();
        // DPC and ISR share the PerfInfo GUID; OR their flags (DPC 0x20 | INTERRUPT 0x40).
        let perf_def = KernelProvider::new(kernel_providers::DPC_PROVIDER.guid, 0x20 | 0x40);
        let perf = Provider::kernel(&perf_def).add_callback(handler(state.clone(), Kind::PerfInfo)).build();

        // A crashed earlier run leaves its session alive (and the name taken); clear it first.
        let _ = ferrisetw::trace::stop_trace_by_name(name);

        let trace = KernelTrace::new()
            .named(name.to_string())
            // ferrisetw defaults to 32 KB buffers; events run at 10^5/s, so go bigger.
            // (ETW buffers are nonpaged memory: 128 KB x 128 = 16 MB worst case.)
            .set_trace_properties(TraceProperties {
                buffer_size: 128,
                min_buffer: 32,
                max_buffer: 128,
                ..Default::default()
            })
            .enable(thread)
            .enable(file)
            .enable(disk)
            .enable(perf)
            .start_and_process()
            .map_err(|e| format!("kernel trace did not start ({e:?}); needs an elevated process"))?;

        // Names for files opened before the trace come from the system handle table.
        let stop = Arc::new(AtomicBool::new(false));
        let resolver = {
            let (state, stop) = (state.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // Sleep in short steps so stop() isn't held up for a full second.
                    for _ in 0..10 {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    resolve_pass(&state);
                }
            })
        };

        Ok(Self {
            name: name.to_string(),
            state,
            trace: Some(trace),
            stop,
            resolver: Some(resolver),
            last_take: Mutex::new(Instant::now()),
            qpc_freq,
        })
    }

    /// Returns what happened since the previous call (or since start) and starts a new interval.
    pub fn take_interval(&self) -> Interval {
        let mut s = self.state.lock().unwrap();
        // Objects the resolver already tried and failed on will never get a name: report their
        // bytes now (under UNRESOLVED) rather than holding them until the file closes.
        let tried: Vec<usize> =
            s.unnamed_bytes.keys().filter(|o| s.resolve_tried.contains(*o)).copied().collect();
        for o in tried {
            s.flush_obj(o);
        }
        let mut iv = std::mem::take(&mut s.cur);
        let mut last = self.last_take.lock().unwrap();
        iv.elapsed = last.elapsed();
        *last = Instant::now();
        iv
    }

    pub fn health(&self) -> Option<SessionHealth> {
        query_session(&self.name)
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let s = self.state.lock().unwrap();
        let mut seen: Vec<_> =
            s.seen.iter().map(|((k, op), (n, c))| (format!("{k:?}"), *op, n.clone(), *c)).collect();
        seen.sort();
        let mut parse_errors: Vec<_> = s.parse_errors.iter().map(|(k, v)| (k.clone(), *v)).collect();
        parse_errors.sort();
        Diagnostics {
            seen,
            parse_errors,
            schema_errors: s.schema_errors,
            names_from_create: s.names_from_create,
            names_from_handles: s.names_from_handles,
        }
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(r) = self.resolver.take() {
            let _ = r.join();
        }
        if let Some(t) = self.trace.take() {
            if let Err(e) = t.stop() {
                eprintln!("stopping trace '{}': {e:?}", self.name);
            }
        }
    }

    /// Stops the session. (Dropping does the same, but `process::exit` skips destructors, so the
    /// collector calls this explicitly on its way out.)
    pub fn stop(mut self) {
        self.shutdown();
    }
}

impl Drop for TraceSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Sorted (base address, driver file name) for every loaded kernel module.
pub fn driver_table() -> Vec<(usize, String)> {
    let mut bases: Vec<*mut c_void> = vec![std::ptr::null_mut(); 4096];
    let mut needed = 0u32;
    let bytes = (bases.len() * std::mem::size_of::<*mut c_void>()) as u32;
    if unsafe { EnumDeviceDrivers(bases.as_mut_ptr(), bytes, &mut needed) }.is_err() {
        return Vec::new();
    }
    let n = (needed as usize / std::mem::size_of::<*mut c_void>()).min(bases.len());
    let mut out = Vec::with_capacity(n);
    for &b in &bases[..n] {
        let mut buf = [0u16; 260];
        let len = unsafe { GetDeviceDriverBaseNameW(b, &mut buf) } as usize;
        if len > 0 {
            out.push((b as usize, String::from_utf16_lossy(&buf[..len])));
        }
    }
    out.sort_by_key(|(a, _)| *a);
    out
}

/// Maps a code address to the driver whose image contains it (nearest base below, within 64 MB).
pub fn owner<'a>(table: &'a [(usize, String)], addr: usize) -> &'a str {
    let i = table.partition_point(|(b, _)| *b <= addr);
    if i == 0 {
        return "?";
    }
    let (base, name) = &table[i - 1];
    if addr - base > 64 * 1024 * 1024 {
        "?"
    } else {
        name
    }
}
