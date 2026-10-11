//! `collector discover [--emit FILE]`: works out this Windows build's ETW event tables from the
//! running system, then compares them with the profile that would be used.
//!
//! What it does:
//! 1. Runs a short kernel trace and records every event type the build sends, with the opcode
//!    *name* and field names Windows reports for it (TDH), so roles are bound by name, not number.
//! 2. Runs a known file workload in this process and reads off facts from the events it caused:
//!    which `IoFlags` bits mark cache-flush (paging) I/O, which status ends a read at end of file,
//!    whether event timestamps are on the same clock as DPC/ISR start times.
//! 3. Compares each discovered value with the profile and says MATCH / SUBSET / EXTRA / MISMATCH /
//!    NOT FOUND, so a human can decide what to trust.
//!
//! What it does NOT do: search for provider flags (they are inputs here, taken from the profile; it
//! only checks that they produce every event role), or discover opaque structures (memory lists,
//! handle table), which have no self-description and rely on the runtime invariant checks.

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrisetw::parser::{Parser, Pointer};
use ferrisetw::provider::kernel_providers::{self, KernelProvider};
use ferrisetw::provider::Provider;
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::{KernelTrace, TraceProperties};
use ferrisetw::EventRecord;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::params::EtwParams;

const SESSION: &str = "ProcLensDiscover";

// Workload steps. Events are delivered in batches about a second after they happen, so a step is
// attributed to an event by the event's own QPC timestamp against the step's recorded start time,
// never by what the workload was doing when the callback happened to run.
const PHASE_CACHED_WRITE: u8 = 1;
const PHASE_FLUSH: u8 = 2;
const PHASE_READ: u8 = 3;
/// Control: an unbuffered write sets the no-cache bit without being paging I/O, so any bit it shares
/// with the flush writes cannot be what marks paging.
const PHASE_UNBUFFERED: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Family {
    Thread,
    File,
    Disk,
    Perf,
}

#[derive(Debug, Default, Clone)]
struct OpInfo {
    name: String,
    count: u64,
    fields: Vec<String>,
    /// Whether the survey parses this event's values (read/write/end, name events, DPC/ISR).
    interesting: bool,
}

/// One read or write this process made, as the trace saw it.
#[derive(Debug, Clone)]
struct MyIo {
    write: bool,
    /// The event's own QPC timestamp (header time with the vendored raw-timestamp patch).
    ts: i64,
    /// Bytes requested; tiny writes (this process's own console/log output) are noise for calibration.
    size: u64,
    io_flags: u32,
    status: Option<u32>,
}

#[derive(Default)]
struct Survey {
    ops: HashMap<(Family, u8), OpInfo>,
    my_io: Vec<MyIo>,
    /// IrpPtr of this process's requests -> index into `my_io`, to attach completions.
    pending: HashMap<usize, usize>,
    /// Name-bearing events: (parsed FileName ok, failed), by opcode.
    name_parse: HashMap<u8, (u64, u64)>,
    create_path_parse: (u64, u64),
    /// DPC/ISR timing sanity by opcode: (events, negative, implausible >1 s).
    perf_timing: HashMap<u8, (u64, u64, u64)>,
    qpc_freq: i64,
    /// (phase, QPC time it started), in order.
    phase_marks: Vec<(u8, i64)>,
}

impl Survey {
    fn phase_of(&self, ts: i64) -> u8 {
        self.phase_marks.iter().rev().find(|(_, start)| *start <= ts).map_or(0, |(p, _)| *p)
    }
}

fn is_interesting(f: Family, name: &str) -> bool {
    match f {
        Family::File => matches!(
            name,
            "Read" | "Write" | "OperationEnd" | "Create" | "Name" | "FileCreate" | "FileDelete" | "FileRundown"
        ),
        Family::Perf => name.contains("DPC") || name.starts_with("ISR"),
        Family::Thread | Family::Disk => false,
    }
}

fn handler(
    state: Arc<Mutex<Survey>>,
    family: Family,
) -> impl FnMut(&EventRecord, &SchemaLocator) + Send + Sync + 'static {
    let me = std::process::id();
    move |record, locator| {
        let op = record.opcode();
        let known = state.lock().unwrap().ops.get(&(family, op)).map(|i| i.interesting);
        if known == Some(false) {
            state.lock().unwrap().ops.get_mut(&(family, op)).unwrap().count += 1;
            return;
        }
        let Ok(schema) = locator.event_schema(record) else { return };
        let name = schema.opcode_name();
        {
            let mut s = state.lock().unwrap();
            let info = s.ops.entry((family, op)).or_insert_with(|| OpInfo {
                name: name.clone(),
                count: 0,
                fields: schema.property_names(),
                interesting: is_interesting(family, &name),
            });
            info.count += 1;
            if !info.interesting {
                return;
            }
        }
        let parser = Parser::create(record, &schema);
        let mut s = state.lock().unwrap();
        match (family, name.as_str()) {
            (Family::File, "Read" | "Write") => {
                if record.process_id() == me {
                    if let (Some(irp), flags) =
                        (parser.try_parse::<Pointer>("IrpPtr").ok(), parser.try_parse::<u32>("IoFlags").ok())
                    {
                        let idx = s.my_io.len();
                        s.my_io.push(MyIo {
                            write: name == "Write",
                            ts: record.raw_timestamp(),
                            size: parser.try_parse::<u32>("IoSize").unwrap_or(0) as u64,
                            io_flags: flags.unwrap_or(0),
                            status: None,
                        });
                        s.pending.insert(*irp, idx);
                    }
                }
            }
            (Family::File, "OperationEnd") => {
                if let Ok(irp) = parser.try_parse::<Pointer>("IrpPtr") {
                    if let Some(idx) = s.pending.remove(&*irp) {
                        s.my_io[idx].status = parser.try_parse::<u32>("NtStatus").ok();
                    }
                }
            }
            (Family::File, "Create") => {
                let ok = parser.try_parse::<String>("OpenPath").is_ok();
                if ok { s.create_path_parse.0 += 1 } else { s.create_path_parse.1 += 1 }
            }
            (Family::File, _) => {
                // Name / FileCreate / FileDelete / FileRundown
                let ok = parser.try_parse::<String>("FileName").is_ok();
                let e = s.name_parse.entry(op).or_default();
                if ok { e.0 += 1 } else { e.1 += 1 }
            }
            (Family::Perf, _) => {
                if let Ok(init) = parser.try_parse::<u64>("InitialTime") {
                    let d = record.raw_timestamp() - init as i64;
                    let limit = s.qpc_freq;
                    let e = s.perf_timing.entry(op).or_default();
                    e.0 += 1;
                    if d < 0 {
                        e.1 += 1;
                    } else if d > limit {
                        e.2 += 1;
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Role binding: from what the build reported (names, fields) to the same shape as the profile.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
pub struct Discovered {
    pub thread_start: Vec<u8>,
    pub file_names: Vec<u8>,
    pub create: Vec<u8>,
    pub close: Vec<u8>,
    pub read: Vec<u8>,
    pub write: Vec<u8>,
    pub end: Vec<u8>,
    pub disk_read: Vec<u8>,
    pub disk_write: Vec<u8>,
    pub dpc: Vec<u8>,
    pub isr: Vec<u8>,
    /// Role -> fields the profile code needs that the event does not report.
    pub missing_fields: Vec<String>,
}

fn opcodes_named(ops: &HashMap<(Family, u8), OpInfo>, f: Family, names: &[&str]) -> Vec<u8> {
    let mut v: Vec<u8> = ops
        .iter()
        .filter(|((fam, _), i)| *fam == f && names.contains(&i.name.as_str()))
        .map(|((_, op), _)| *op)
        .collect();
    v.sort_unstable();
    v
}

fn opcodes_where(ops: &HashMap<(Family, u8), OpInfo>, f: Family, pred: impl Fn(&str) -> bool) -> Vec<u8> {
    let mut v: Vec<u8> =
        ops.iter().filter(|((fam, _), i)| *fam == f && pred(&i.name)).map(|((_, op), _)| *op).collect();
    v.sort_unstable();
    v
}

/// Fields `trace.rs` parses for each role.
fn required_fields(role: &str) -> &'static [&'static str] {
    match role {
        "thread.start" => &["ProcessId", "TThreadId"],
        "file.name" => &["FileObject", "FileName"],
        "file.create" => &["FileObject", "OpenPath"],
        "file.close" => &["FileObject"],
        "file.read" | "file.write" => &["IrpPtr", "FileObject", "IoSize", "IoFlags"],
        "file.operation_end" => &["IrpPtr", "ExtraInfo", "NtStatus"],
        "disk.read" | "disk.write" => &["TransferSize", "HighResResponseTime", "IssuingThreadId"],
        "perf.dpc" | "perf.isr" => &["InitialTime", "Routine"],
        _ => &[],
    }
}

fn bind_roles(ops: &HashMap<(Family, u8), OpInfo>) -> Discovered {
    let mut d = Discovered {
        thread_start: opcodes_named(ops, Family::Thread, &["Start", "DCStart"]),
        file_names: opcodes_named(ops, Family::File, &["Name", "FileCreate", "FileDelete", "FileRundown"]),
        create: opcodes_named(ops, Family::File, &["Create"]),
        close: opcodes_named(ops, Family::File, &["Close"]),
        read: opcodes_named(ops, Family::File, &["Read"]),
        write: opcodes_named(ops, Family::File, &["Write"]),
        end: opcodes_named(ops, Family::File, &["OperationEnd"]),
        disk_read: opcodes_named(ops, Family::Disk, &["Read"]),
        disk_write: opcodes_named(ops, Family::Disk, &["Write"]),
        dpc: opcodes_where(ops, Family::Perf, |n| n.contains("DPC")),
        isr: opcodes_where(ops, Family::Perf, |n| n.starts_with("ISR")),
        missing_fields: Vec::new(),
    };
    let checks: [(&str, Family, &Vec<u8>); 11] = [
        ("thread.start", Family::Thread, &d.thread_start),
        ("file.name", Family::File, &d.file_names),
        ("file.create", Family::File, &d.create),
        ("file.close", Family::File, &d.close),
        ("file.read", Family::File, &d.read),
        ("file.write", Family::File, &d.write),
        ("file.operation_end", Family::File, &d.end),
        ("disk.read", Family::Disk, &d.disk_read),
        ("disk.write", Family::Disk, &d.disk_write),
        ("perf.dpc", Family::Perf, &d.dpc),
        ("perf.isr", Family::Perf, &d.isr),
    ];
    let mut missing = Vec::new();
    for (role, fam, opcodes) in checks {
        for op in opcodes {
            let have = &ops[&(fam, *op)].fields;
            for need in required_fields(role) {
                if !have.iter().any(|f| f == need) {
                    missing.push(format!("{role} (op {op} '{}') lacks field {need}", ops[&(fam, *op)].name));
                }
            }
        }
    }
    d.missing_fields = missing;
    d
}

// ---------------------------------------------------------------------------------------------
// Comparison with the profile.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Match,
    /// Everything discovered is in the profile, but the profile lists more (not seen in this window).
    Subset,
    /// Discovered something the profile doesn't list.
    Extra,
    Mismatch,
    NotFound,
    /// The workload couldn't exercise this.
    Inconclusive,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Match => "MATCH",
            Verdict::Subset => "SUBSET",
            Verdict::Extra => "EXTRA",
            Verdict::Mismatch => "MISMATCH",
            Verdict::NotFound => "NOT FOUND",
            Verdict::Inconclusive => "INCONCLUSIVE",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub item: String,
    pub profile: String,
    pub discovered: String,
    pub verdict: Verdict,
    pub note: String,
}

fn list(v: &[u8]) -> String {
    if v.is_empty() {
        "-".to_string()
    } else {
        v.iter().map(|o| o.to_string()).collect::<Vec<_>>().join(", ")
    }
}

fn compare_set(item: &str, profile: &[u8], found: &[u8]) -> Row {
    let p: BTreeSet<u8> = profile.iter().copied().collect();
    let f: BTreeSet<u8> = found.iter().copied().collect();
    let (verdict, note) = if f.is_empty() {
        (Verdict::NotFound, "no event with a matching name was observed".to_string())
    } else if p == f {
        (Verdict::Match, String::new())
    } else if f.is_subset(&p) {
        let missing: Vec<_> = p.difference(&f).map(|o| o.to_string()).collect();
        (Verdict::Subset, format!("profile also lists {} (not seen in this window)", missing.join(", ")))
    } else if p.is_subset(&f) {
        let extra: Vec<_> = f.difference(&p).map(|o| o.to_string()).collect();
        (Verdict::Extra, format!("observed but not in profile: {}", extra.join(", ")))
    } else {
        (Verdict::Mismatch, "the sets differ in both directions".to_string())
    };
    Row { item: item.to_string(), profile: list(profile), discovered: list(found), verdict, note }
}

fn bits(v: u32) -> String {
    format!("{v:#06x}")
}

/// Calibrated facts that come from the workload rather than from event names.
#[derive(Debug, Default)]
pub struct Calibrated {
    /// Bits set on every flush-phase write and on no cached-write-phase write.
    pub paging_bits: Option<u32>,
    /// Bits the flush writes share with the unbuffered control writes (e.g. IRP_NOCACHE): not paging.
    pub shared_with_control: u32,
    pub control_writes: usize,
    pub flush_writes: usize,
    pub cached_writes: usize,
    /// Non-zero completion statuses seen on this process's reads, with how often.
    pub read_statuses: Vec<(u32, usize)>,
    /// Whether DPC/ISR (event time - InitialTime) behaved like QPC ticks: no negatives, some events.
    pub raw_timestamps_ok: Option<bool>,
    pub timing_events: u64,
    pub timing_negative: u64,
}

const MIN_SIGNIFICANT: u64 = 4096;

/// Bits set on writes that account for at least `percent` of the bytes. By bytes, not by event
/// count, so a few small metadata writes can neither create nor veto a bit.
fn bits_with_share(events: &[&MyIo], percent: u64) -> u32 {
    let total: u64 = events.iter().map(|e| e.size).sum();
    if total == 0 {
        return 0;
    }
    (0..32)
        .map(|b| 1u32 << b)
        .filter(|bit| {
            events.iter().filter(|e| e.io_flags & bit != 0).map(|e| e.size).sum::<u64>() * 100 >= total * percent
        })
        .fold(0, |acc, bit| acc | bit)
}

/// For printing: distinct `IoFlags` values among this process's writes, by phase: (phase, flags, writes, bytes).
fn flags_by_phase(s: &Survey) -> Vec<(u8, u32, usize, u64)> {
    let mut m: std::collections::BTreeMap<(u8, u32), (usize, u64)> = Default::default();
    for e in s.my_io.iter().filter(|e| e.write && e.size >= MIN_SIGNIFICANT) {
        let slot = m.entry((s.phase_of(e.ts), e.io_flags)).or_default();
        slot.0 += 1;
        slot.1 += e.size;
    }
    m.into_iter().map(|((p, f), (n, b))| (p, f, n, b)).collect()
}

fn calibrate(s: &Survey) -> Calibrated {
    let mut c = Calibrated::default();

    // Only writes of at least a page: stray small writes from this process (its own output) carry
    // ordinary flags and would otherwise veto bits that every real flush write has.
    let writes = |phase: u8| {
        s.my_io
            .iter()
            .filter(|e| e.write && e.size >= MIN_SIGNIFICANT && s.phase_of(e.ts) == phase)
            .collect::<Vec<_>>()
    };
    let cached = writes(PHASE_CACHED_WRITE);
    let flush = writes(PHASE_FLUSH);
    c.cached_writes = cached.len();
    c.flush_writes = flush.len();
    let control = writes(PHASE_UNBUFFERED);
    c.control_writes = control.len();
    if !flush.is_empty() {
        // Paging bits = what (almost) all flushed bytes carry (>= 90%) minus what a real share
        // (>= 10%) of ordinary data writes carry. The control writes also bring small paging
        // writes for filesystem metadata into their window; at a few percent of the bytes they
        // must not count as "what an unbuffered write looks like".
        let flush_bits = bits_with_share(&flush, 90);
        let ordinary_cached = bits_with_share(&cached, 10);
        let ordinary_control = bits_with_share(&control, 10);
        c.shared_with_control = flush_bits & ordinary_control & !ordinary_cached;
        c.paging_bits = Some(flush_bits & !ordinary_cached & !ordinary_control);
    }

    let mut st: HashMap<u32, usize> = HashMap::new();
    for e in s.my_io.iter().filter(|e| !e.write && s.phase_of(e.ts) == PHASE_READ) {
        if let Some(status) = e.status.filter(|s| *s != 0) {
            *st.entry(status).or_default() += 1;
        }
    }
    c.read_statuses = st.into_iter().collect();
    c.read_statuses.sort();

    for (n, neg, _imp) in s.perf_timing.values() {
        c.timing_events += n;
        c.timing_negative += neg;
    }
    if c.timing_events > 0 {
        c.raw_timestamps_ok = Some(c.timing_negative == 0);
    }
    c
}

fn compare(profile: &EtwParams, d: &Discovered, c: &Calibrated, s: &Survey) -> Vec<Row> {
    let f = &profile.file;
    let mut rows = vec![
        compare_set("thread.start_opcodes", &profile.thread.start_opcodes, &d.thread_start),
        compare_set("file.name_opcodes", &f.name_opcodes, &d.file_names),
        compare_set("file.create_opcode", &[f.create_opcode], &d.create),
        compare_set("file.close_opcode", &[f.close_opcode], &d.close),
        compare_set("file.read_opcode", &[f.read_opcode], &d.read),
        compare_set("file.write_opcode", &[f.write_opcode], &d.write),
        compare_set("file.operation_end_opcode", &[f.operation_end_opcode], &d.end),
        compare_set("disk.read_opcode", &[profile.disk.read_opcode], &d.disk_read),
        compare_set("disk.write_opcode", &[profile.disk.write_opcode], &d.disk_write),
        compare_set("perf.dpc_opcodes", &profile.perf.dpc_opcodes, &d.dpc),
        compare_set("perf.isr_opcodes", &profile.perf.isr_opcodes, &d.isr),
    ];

    // Paging mask: the workload shows which bits mark flush traffic.
    let p = f.paging_io_flags.0;
    rows.push(match c.paging_bits {
        None => Row {
            item: "file.paging_io_flags".into(),
            profile: bits(p),
            discovered: "-".into(),
            verdict: Verdict::Inconclusive,
            note: format!("the flush produced no write events from this process ({} cached writes seen)", c.cached_writes),
        },
        Some(0) => Row {
            item: "file.paging_io_flags".into(),
            profile: bits(p),
            discovered: bits(0),
            verdict: Verdict::Mismatch,
            note: "flush writes carried no bit that cached writes lacked".into(),
        },
        Some(found) => {
            let control_note = if c.shared_with_control != 0 {
                format!("{} also set on the unbuffered control writes, so excluded (no-cache, not paging)", bits(c.shared_with_control))
            } else if c.control_writes == 0 {
                "no unbuffered control writes were seen, so no-cache bits could not be ruled out".to_string()
            } else {
                String::new()
            };
            let (verdict, note) = if found == p {
                (Verdict::Match, control_note)
            } else if found & !p == 0 {
                (Verdict::Subset, format!("profile also includes {} (taken from the headers; not exercised here)", bits(p & !found)))
            } else if p & !found == 0 {
                (Verdict::Extra, format!("flush writes also carry {} which the profile does not treat as paging", bits(found & !p)))
            } else {
                (Verdict::Mismatch, "different bits".into())
            };
            Row { item: "file.paging_io_flags".into(), profile: bits(p), discovered: bits(found), verdict, note }
        }
    });

    // End-of-file status: what completes this process's final read of a file.
    let eof = f.end_of_file_ntstatus.0;
    let found: Vec<String> = c.read_statuses.iter().map(|(s, n)| format!("{s:#010x} x{n}")).collect();
    rows.push(match c.read_statuses.as_slice() {
        [] => Row {
            item: "file.end_of_file_ntstatus".into(),
            profile: format!("{eof:#010x}"),
            discovered: "-".into(),
            verdict: Verdict::NotFound,
            note: "no non-zero read completion seen (expected one per read-to-end)".into(),
        },
        [(only, _)] => Row {
            item: "file.end_of_file_ntstatus".into(),
            profile: format!("{eof:#010x}"),
            discovered: found.join(", "),
            verdict: if *only == eof { Verdict::Match } else { Verdict::Mismatch },
            note: String::new(),
        },
        many => Row {
            item: "file.end_of_file_ntstatus".into(),
            profile: format!("{eof:#010x}"),
            discovered: found.join(", "),
            verdict: if many.iter().any(|(s, _)| *s == eof) { Verdict::Extra } else { Verdict::Mismatch },
            note: "more than one non-zero status on reads".into(),
        },
    });

    rows.push(Row {
        item: "raw_timestamps".into(),
        profile: profile.raw_timestamps.to_string(),
        discovered: c.raw_timestamps_ok.map_or("-".to_string(), |b| b.to_string()),
        verdict: match c.raw_timestamps_ok {
            None => Verdict::Inconclusive,
            Some(b) if b == profile.raw_timestamps => Verdict::Match,
            Some(_) => Verdict::Mismatch,
        },
        note: format!("{} DPC/ISR events timed, {} negative durations", c.timing_events, c.timing_negative),
    });

    // Fields: does every bound event report the fields the code reads?
    rows.push(Row {
        item: "event field names".into(),
        profile: "fixed in trace.rs".into(),
        discovered: if d.missing_fields.is_empty() { "all present".into() } else { format!("{} missing", d.missing_fields.len()) },
        verdict: if d.missing_fields.is_empty() { Verdict::Match } else { Verdict::Mismatch },
        note: d.missing_fields.join("; "),
    });

    // Name events: are they actually delivered, with a parsable FileName?
    let parsed: u64 = s.name_parse.values().map(|(ok, _)| ok).sum();
    let failed: u64 = s.name_parse.values().map(|(_, bad)| bad).sum();
    rows.push(Row {
        item: "name events deliver FileName".into(),
        profile: format!("opcodes {}", list(&f.name_opcodes)),
        discovered: format!("{parsed} parsed, {failed} failed"),
        verdict: if parsed > 0 && failed == 0 { Verdict::Match } else if parsed > 0 { Verdict::Subset } else { Verdict::NotFound },
        note: "per opcode: ".to_string()
            + &s.name_parse
                .iter()
                .map(|(op, (ok, bad))| format!("{op}: {ok} ok/{bad} bad"))
                .collect::<Vec<_>>()
                .join(", "),
    });
    rows
}

// ---------------------------------------------------------------------------------------------
// Running it.
// ---------------------------------------------------------------------------------------------

/// Writes 4 MiB with FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH from a sector-aligned buffer.
fn unbuffered_write(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const NO_BUFFERING: u32 = 0x2000_0000;
    const WRITE_THROUGH: u32 = 0x8000_0000;
    const MIB: usize = 1 << 20;
    let layout = std::alloc::Layout::from_size_align(MIB, 4096).expect("valid layout");
    // Unbuffered I/O needs a sector-aligned buffer, which a Vec<u8> doesn't promise.
    let buf = unsafe { std::alloc::alloc_zeroed(layout) };
    if buf.is_null() {
        return Err(std::io::Error::other("allocation failed"));
    }
    let result = (|| {
        // create_new: this process is elevated and the path is in a folder an unprivileged user can
        // write to, so never open or truncate something that is already there.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(NO_BUFFERING | WRITE_THROUGH)
            .open(path)?;
        for _ in 0..4 {
            f.write_all(unsafe { std::slice::from_raw_parts(buf, MIB) })?;
        }
        Ok(())
    })();
    unsafe { std::alloc::dealloc(buf, layout) };
    let _ = std::fs::remove_file(path);
    result
}

fn workload(state: &Mutex<Survey>) -> std::io::Result<()> {
    let mark = |phase: u8| {
        let mut now = 0i64;
        let _ = unsafe { QueryPerformanceCounter(&mut now) };
        state.lock().unwrap().phase_marks.push((phase, now));
    };
    const MIB: usize = 1 << 20;
    let (mut f, path) = crate::profile::create_private_temp("proclens-discover", "bin")?;
    let chunk = vec![0x3Cu8; MIB];

    mark(PHASE_CACHED_WRITE);
    for _ in 0..8 {
        f.write_all(&chunk)?;
    }
    std::thread::sleep(Duration::from_millis(400)); // let the cached writes' events land

    mark(PHASE_FLUSH);
    f.sync_all()?;
    drop(f);
    std::thread::sleep(Duration::from_millis(600));

    mark(PHASE_UNBUFFERED);
    let control = unbuffered_write(&path.with_extension("ub.bin"));
    std::thread::sleep(Duration::from_millis(600));
    if let Err(e) = control {
        eprintln!("note: the unbuffered control write failed ({e}); paging bits can't be separated from no-cache");
    }

    mark(PHASE_READ);
    for _ in 0..2 {
        let mut f = std::fs::File::open(&path)?;
        let mut buf = vec![0u8; MIB];
        while f.read(&mut buf)? > 0 {} // read to end of file, twice
    }
    std::thread::sleep(Duration::from_millis(400));
    mark(0);
    let _ = std::fs::remove_file(&path);
    Ok(())
}

pub fn run(emit: Option<PathBuf>) {
    let selection = crate::profile::etw_selection();
    let profile = &selection.params;
    println!("== ProcLens discovery on this PC ==");
    let os = crate::profile::os_info();
    println!(
        "Windows {}.{} build {}.{} ({}, {}), profile tables: {}",
        os.major, os.minor, os.build, os.ubr, os.display_version, os.edition, selection.source
    );
    println!("Provider flags are taken from the profile (file {:#x}, perf {:#x}); discovery checks they yield every event role.\n",
        profile.file_flags(), profile.perf_flags());

    let mut freq = 0i64;
    let _ = unsafe { QueryPerformanceFrequency(&mut freq) };
    let state = Arc::new(Mutex::new(Survey { qpc_freq: freq.max(1), ..Default::default() }));

    let thread = Provider::kernel(&kernel_providers::THREAD_PROVIDER)
        .add_callback(handler(state.clone(), Family::Thread))
        .build();
    let file_def = KernelProvider::new(kernel_providers::FILE_IO_PROVIDER.guid, profile.file_flags());
    let file = Provider::kernel(&file_def).add_callback(handler(state.clone(), Family::File)).build();
    let disk = Provider::kernel(&kernel_providers::DISK_IO_PROVIDER)
        .add_callback(handler(state.clone(), Family::Disk))
        .build();
    let perf_def = KernelProvider::new(kernel_providers::DPC_PROVIDER.guid, profile.perf_flags());
    let perf = Provider::kernel(&perf_def).add_callback(handler(state.clone(), Family::Perf)).build();

    let _ = ferrisetw::trace::stop_trace_by_name(SESSION);
    let trace = match KernelTrace::new()
        .named(SESSION.to_string())
        .set_trace_properties(TraceProperties { buffer_size: 128, min_buffer: 32, max_buffer: 128, ..Default::default() })
        .enable(thread)
        .enable(file)
        .enable(disk)
        .enable(perf)
        .start_and_process()
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not start the kernel trace ({e:?}); discovery needs an elevated process");
            return;
        }
    };

    std::thread::sleep(Duration::from_millis(1200)); // let the session settle
    println!("Surveying events and running a known file workload (about 5 s)...");
    if let Err(e) = workload(&state) {
        eprintln!("workload failed: {e}");
    }
    std::thread::sleep(Duration::from_millis(1200));
    let _ = trace.stop();

    let s = state.lock().unwrap();
    println!("\n== Event types this build sent (family / opcode / name / count / fields) ==");
    let mut all: Vec<_> = s.ops.iter().collect();
    all.sort_by_key(|((f, op), _)| (format!("{f:?}"), *op));
    for ((f, op), i) in all {
        println!("{:<7} {:>3}  {:<18} {:>8}  {}", format!("{f:?}"), op, i.name, i.count, i.fields.join(" "));
    }

    let d = bind_roles(&s.ops);
    let c = calibrate(&s);
    println!(
        "\nCalibration workload: {} cached writes, {} writes during flush, reads ended with {:?}",
        c.cached_writes,
        c.flush_writes,
        c.read_statuses.iter().map(|(st, n)| format!("{st:#x} x{n}")).collect::<Vec<_>>()
    );

    println!("Significant (>= 4 KiB) writes by this process, grouped by workload step and IoFlags:");
    for (phase, flags, n, bytes) in flags_by_phase(&s) {
        let step = match phase {
            PHASE_CACHED_WRITE => "cached write",
            PHASE_FLUSH => "flush",
            PHASE_UNBUFFERED => "unbuffered control",
            PHASE_READ => "read",
            _ => "outside the workload",
        };
        println!("  {step:<22} IoFlags {flags:#06x}  {n:>4} writes  {:>6.1} MiB", bytes as f64 / 1048576.0);
    }

    let rows = compare(profile, &d, &c, &s);
    println!("\n== Discovery vs profile ==");
    println!("{:<30} {:<18} {:<24} {:<13} note", "item", "profile", "discovered", "verdict");
    for r in &rows {
        println!("{:<30} {:<18} {:<24} {:<13} {}", r.item, r.profile, r.discovered, r.verdict.label(), r.note);
    }
    let count = |v: Verdict| rows.iter().filter(|r| r.verdict == v).count();
    println!(
        "\nSummary: {} match, {} subset, {} extra, {} mismatch, {} not found, {} inconclusive",
        count(Verdict::Match),
        count(Verdict::Subset),
        count(Verdict::Extra),
        count(Verdict::Mismatch),
        count(Verdict::NotFound),
        count(Verdict::Inconclusive)
    );

    if let Some(path) = emit {
        let json = draft_json(&os, &d, &c, profile);
        match std::fs::write(&path, json) {
            Ok(()) => println!("Draft etw section written to {} (review before using; see notes inside).", path.display()),
            Err(e) => eprintln!("could not write {}: {e}", path.display()),
        }
    }
}

/// The `etw` section as discovery sees it. Values discovery could not determine are copied from the
/// profile it was compared against, and the notes say so.
fn draft_json(os: &proclens_proto::OsInfo, d: &Discovered, c: &Calibrated, profile: &EtwParams) -> String {
    let one = |v: &[u8], fallback: u8| v.first().copied().unwrap_or(fallback);
    let or_profile = |v: &[u8], fb: &[u8]| if v.is_empty() { fb.to_vec() } else { v.to_vec() };
    let paging = c.paging_bits.filter(|b| *b != 0).unwrap_or(profile.file.paging_io_flags.0);
    let eof = match c.read_statuses.as_slice() {
        [(s, _)] => *s,
        _ => profile.file.end_of_file_ntstatus.0,
    };
    let v = serde_json::json!({
        "raw_timestamps": c.raw_timestamps_ok.unwrap_or(profile.raw_timestamps),
        "notes": [
            format!("DRAFT from `collector discover` on Windows {}.{} build {}.{}.", os.major, os.minor, os.build, os.ubr),
            "Opcodes were bound by name from live events; values discovery could not determine were copied from the profile used for comparison.",
            "Provider flags are copied from that profile: discovery does not search for them.",
            "Review against docs/REFERENCES.md before trusting."
        ],
        "thread": { "start_opcodes": or_profile(&d.thread_start, &profile.thread.start_opcodes) },
        "file": {
            "flags": profile.file.flags.iter().map(|(k, h)| (k.clone(), format!("{:#010x}", h.0))).collect::<std::collections::BTreeMap<_, _>>(),
            "name_opcodes": or_profile(&d.file_names, &profile.file.name_opcodes),
            "create_opcode": one(&d.create, profile.file.create_opcode),
            "close_opcode": one(&d.close, profile.file.close_opcode),
            "read_opcode": one(&d.read, profile.file.read_opcode),
            "write_opcode": one(&d.write, profile.file.write_opcode),
            "operation_end_opcode": one(&d.end, profile.file.operation_end_opcode),
            "paging_io_flags": format!("{paging:#010x}"),
            "end_of_file_ntstatus": format!("{eof:#010x}")
        },
        "disk": {
            "read_opcode": one(&d.disk_read, profile.disk.read_opcode),
            "write_opcode": one(&d.disk_write, profile.disk.write_opcode)
        },
        "perf": {
            "flags": profile.perf.flags.iter().map(|(k, h)| (k.clone(), format!("{:#010x}", h.0))).collect::<std::collections::BTreeMap<_, _>>(),
            "dpc_opcodes": or_profile(&d.dpc, &profile.perf.dpc_opcodes),
            "isr_opcodes": or_profile(&d.isr, &profile.perf.isr_opcodes)
        }
    });
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(name: &str, fields: &[&str]) -> OpInfo {
        OpInfo { name: name.into(), count: 1, fields: fields.iter().map(|s| s.to_string()).collect(), interesting: false }
    }

    /// The event names this build was observed to send (see SPIKE_FINDINGS.md, run of 2026-10-10).
    fn observed_19045() -> HashMap<(Family, u8), OpInfo> {
        let rw = ["Offset", "IrpPtr", "TTID", "FileObject", "FileKey", "IoSize", "IoFlags"];
        let mut m = HashMap::new();
        m.insert((Family::Thread, 1), op("Start", &["ProcessId", "TThreadId"]));
        m.insert((Family::Thread, 2), op("End", &["ProcessId", "TThreadId"]));
        m.insert((Family::Thread, 3), op("DCStart", &["ProcessId", "TThreadId"]));
        m.insert((Family::File, 32), op("FileCreate", &["FileObject", "FileName"]));
        m.insert((Family::File, 35), op("FileDelete", &["FileObject", "FileName"]));
        m.insert((Family::File, 64), op("Create", &["FileObject", "OpenPath"]));
        m.insert((Family::File, 66), op("Close", &["FileObject"]));
        m.insert((Family::File, 67), op("Read", &rw));
        m.insert((Family::File, 68), op("Write", &rw));
        m.insert((Family::File, 76), op("OperationEnd", &["IrpPtr", "ExtraInfo", "NtStatus"]));
        m.insert((Family::Disk, 10), op("Read", &["TransferSize", "HighResResponseTime", "IssuingThreadId"]));
        m.insert((Family::Disk, 11), op("Write", &["TransferSize", "HighResResponseTime", "IssuingThreadId"]));
        m.insert((Family::Disk, 14), op("FlushBuffers", &[]));
        m.insert((Family::Perf, 50), op("ISR-MSI", &["InitialTime", "Routine"]));
        m.insert((Family::Perf, 67), op("ISR", &["InitialTime", "Routine"]));
        m.insert((Family::Perf, 68), op("DPC", &["InitialTime", "Routine"]));
        m.insert((Family::Perf, 69), op("TimerDPC", &["InitialTime", "Routine"]));
        m
    }

    #[test]
    fn roles_are_bound_by_name_not_number() {
        let d = bind_roles(&observed_19045());
        assert_eq!(d.read, vec![67]);
        assert_eq!(d.write, vec![68]);
        assert_eq!(d.end, vec![76]);
        assert_eq!(d.isr, vec![50, 67], "ISR-MSI counts as an ISR");
        assert_eq!(d.dpc, vec![68, 69]);
        assert_eq!(d.file_names, vec![32, 35]);
        assert_eq!(d.thread_start, vec![1, 3]);
        assert!(d.missing_fields.is_empty(), "{:?}", d.missing_fields);

        // The same names under different numbers must bind to the new numbers.
        let mut shifted = observed_19045();
        let read = shifted.remove(&(Family::File, 67)).unwrap();
        shifted.insert((Family::File, 90), read);
        assert_eq!(bind_roles(&shifted).read, vec![90]);
    }

    #[test]
    fn a_missing_field_is_reported_against_its_role() {
        let mut m = observed_19045();
        m.get_mut(&(Family::File, 76)).unwrap().fields.retain(|f| f != "ExtraInfo");
        let d = bind_roles(&m);
        assert_eq!(d.missing_fields.len(), 1);
        assert!(d.missing_fields[0].contains("file.operation_end") && d.missing_fields[0].contains("ExtraInfo"));
    }

    #[test]
    fn set_comparison_distinguishes_the_outcomes() {
        assert_eq!(compare_set("x", &[66, 68, 69], &[66, 68, 69]).verdict, Verdict::Match);
        let sub = compare_set("x", &[66, 68, 69], &[68, 69]);
        assert_eq!(sub.verdict, Verdict::Subset);
        assert!(sub.note.contains("66"));
        assert_eq!(compare_set("x", &[68], &[68, 99]).verdict, Verdict::Extra);
        assert_eq!(compare_set("x", &[68], &[99]).verdict, Verdict::Mismatch);
        assert_eq!(compare_set("x", &[68], &[]).verdict, Verdict::NotFound);
    }

    #[test]
    fn paging_bits_come_from_what_flush_writes_have_and_cached_writes_lack() {
        let io = |write, ts, flags| MyIo { write, ts, size: 1 << 20, io_flags: flags, status: None };
        let mut s = Survey::default();
        // Steps started at QPC 100 (cached write), 200 (flush), 300 (read). Events carry their own
        // timestamps, so an event delivered late is still attributed to the step it happened in.
        s.phase_marks =
            vec![(PHASE_CACHED_WRITE, 100), (PHASE_FLUSH, 200), (PHASE_UNBUFFERED, 300), (PHASE_READ, 400)];
        s.my_io = vec![io(true, 110, 0x0), io(true, 190, 0x4), io(true, 210, 0x43), io(true, 250, 0x43)];
        let c = calibrate(&s);
        assert_eq!(c.paging_bits, Some(0x43), "without a control, no-cache (0x1) looks like paging");
        // Control: an unbuffered write has the no-cache bit but is not paging I/O.
        s.my_io.push(io(true, 310, 0x1));
        // Filesystem metadata paging writes land in the control window too: 8 KiB against 1 MiB.
        s.my_io.push(MyIo { write: true, ts: 320, size: 8192, io_flags: 0x43, status: None });
        let c = calibrate(&s);
        assert_eq!(c.paging_bits, Some(0x42), "0x1 is shared with the control writes, so it is excluded");
        assert_eq!(c.shared_with_control, 0x1);
        // A stray tiny write in the flush window (e.g. this process's own console output) must not veto the bits.
        s.my_io.push(MyIo { write: true, ts: 230, size: 40, io_flags: 0x0, status: None });
        assert_eq!(calibrate(&s).paging_bits, Some(0x42), "tiny writes are ignored");
        s.my_io.retain(|e| e.ts < 200);
        assert_eq!(calibrate(&s).paging_bits, None, "no flush events means inconclusive");
    }

    #[test]
    fn eof_status_is_the_nonzero_status_on_reads() {
        let mut s = Survey::default();
        s.phase_marks = vec![(PHASE_READ, 100)];
        s.my_io = (0..4)
            .map(|i| MyIo { write: false, ts: 150 + i, size: 1 << 20, io_flags: 0, status: Some(if i % 2 == 0 { 0 } else { 0xC000_0011 }) })
            .collect();
        assert_eq!(calibrate(&s).read_statuses, vec![(0xC000_0011, 2)]);
    }
}
