//! Per-Windows-build profiles and startup self-tests.
//!
//! ProcLens depends on undocumented structures and trace-event layouts that can differ between
//! Windows builds, so two things are kept separate:
//! - **Profiles** (`profiles/*.json`): what was verified *by hand* on which exact build, plus the
//!   quirks found there. Supporting another PC means adding a file.
//! - **Self-tests** (this module): what provably works on *this* machine *right now*. A failed
//!   test switches the feature off instead of showing wrong numbers, even on a build with no profile.

use proclens_proto::{Capability, CapabilityStatus, OsInfo, SystemProfile, Verification};
use serde::Deserialize;
use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};

/// Profiles compiled into the binary. Add new builds here after verifying them (see
/// `collector profile`, which prints this machine's OS info and self-test results).
const PROFILE_FILES: &[&str] = &[include_str!("../profiles/windows-10-19045.json")];

#[derive(Debug, Deserialize)]
pub struct KnownProfile {
    pub id: String,
    pub title: String,
    pub os: ProfileOs,
    pub verified: ProfileVerified,
    pub verified_capabilities: Vec<String>,
    /// Build-specific trace parameters; kept as data so the ETW code can move to them.
    #[allow(dead_code)]
    pub etw: serde_json::Value,
    pub quirks: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProfileOs {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

#[derive(Debug, Deserialize)]
pub struct ProfileVerified {
    pub ubr: u32,
    pub date: String,
    pub machine: String,
}

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
}

#[repr(C)]
struct OsVersionInfoW {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

fn reg_string(value: PCWSTR) -> String {
    let mut buf = [0u16; 128];
    let mut len = std::mem::size_of_val(&buf) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    };
    if rc.0 != 0 {
        return String::new();
    }
    let chars = (len as usize / 2).saturating_sub(1).min(buf.len()); // drop the NUL
    String::from_utf16_lossy(&buf[..chars])
}

fn reg_dword(value: PCWSTR) -> u32 {
    let (mut data, mut len) = (0u32, 4u32);
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut _ as *mut _),
            Some(&mut len),
        )
    };
    if rc.0 == 0 { data } else { 0 }
}

pub fn os_info() -> OsInfo {
    // RtlGetVersion, unlike GetVersionEx, isn't subject to manifest-based version lying.
    let mut v = OsVersionInfoW { size: std::mem::size_of::<OsVersionInfoW>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    unsafe { RtlGetVersion(&mut v) };
    OsInfo {
        major: v.major,
        minor: v.minor,
        build: v.build,
        ubr: reg_dword(w!("UBR")),
        arch: std::env::consts::ARCH.to_string(),
        display_version: reg_string(w!("DisplayVersion")),
        edition: reg_string(w!("EditionID")),
    }
}

fn known_profiles() -> Vec<KnownProfile> {
    PROFILE_FILES
        .iter()
        .map(|s| serde_json::from_str(s).expect("built-in profile JSON is malformed"))
        .collect()
}

fn find_profile(os: &OsInfo) -> (Verification, Option<KnownProfile>) {
    for p in known_profiles() {
        if p.os.major == os.major && p.os.minor == os.minor && p.os.build == os.build {
            let v = if p.verified.ubr == os.ubr { Verification::Exact } else { Verification::SameBuildOtherPatch };
            return (v, Some(p));
        }
    }
    (Verification::Untested, None)
}

fn check(
    id: &str,
    profile: &Option<KnownProfile>,
    result: Result<String, String>,
) -> Capability {
    let (status, detail) = match result {
        Ok(d) => (CapabilityStatus::Ok, d),
        Err(d) => (CapabilityStatus::Failed, d),
    };
    Capability {
        id: id.to_string(),
        status,
        detail,
        verified_in_profile: profile.as_ref().is_some_and(|p| p.verified_capabilities.iter().any(|c| c == id)),
    }
}

fn not_run(id: &str, profile: &Option<KnownProfile>, why: &str) -> Capability {
    Capability {
        id: id.to_string(),
        status: CapabilityStatus::NotRun,
        detail: why.to_string(),
        verified_in_profile: profile.as_ref().is_some_and(|p| p.verified_capabilities.iter().any(|c| c == id)),
    }
}

const ETW_IDS: [&str; 4] = ["etw_kernel_session", "etw_disk_io", "etw_file_io", "etw_dpc_isr"];

/// Runs a real ~2 s kernel trace with a known file workload and checks what comes out. Event
/// layouts are undocumented and build-specific, so this is what catches a Windows update (or a
/// different build) that changed them: a layout change shows up as parse failures or wrong totals.
fn etw_checks(profile: &Option<KnownProfile>) -> Vec<Capability> {
    use std::io::{Read, Write};
    const MIB: usize = 1 << 20;
    const CHUNKS: usize = 8;

    let session = match crate::trace::TraceSession::start("ProcLensSelfTest") {
        Ok(s) => s,
        Err(e) => return ETW_IDS.iter().map(|id| check(id, profile, Err(e.clone()))).collect(),
    };
    std::thread::sleep(std::time::Duration::from_millis(500)); // let the session settle

    // Known workload: write 8 MiB then read it back, so this process should be credited with
    // exactly that much (plus a little unrelated I/O of its own).
    let name = format!("proclens-selftest-{}.bin", std::process::id());
    let path = std::env::temp_dir().join(&name);
    let workload = (|| -> std::io::Result<()> {
        let chunk = vec![0xA5u8; MIB];
        let mut f = std::fs::File::create(&path)?;
        for _ in 0..CHUNKS {
            f.write_all(&chunk)?;
        }
        f.sync_all()?;
        drop(f);
        let mut f = std::fs::File::open(&path)?;
        let mut buf = vec![0u8; MIB];
        while f.read(&mut buf)? > 0 {}
        Ok(())
    })();
    let _ = std::fs::remove_file(&path);
    std::thread::sleep(std::time::Duration::from_millis(1200)); // let completions and names arrive

    let health = session.health();
    let iv = session.take_interval();
    let diag = session.diagnostics();
    session.stop();

    let errors_in = |prefix: &str| -> Vec<String> {
        diag.parse_errors.iter().filter(|(k, _)| k.starts_with(prefix)).map(|(k, n)| format!("{k} x{n}")).collect()
    };
    let mb = |b: u64| b as f64 / 1048576.0;
    let want = (CHUNKS * MIB) as u64;

    let session_check = match &health {
        Some(h) if h.events_lost == 0 => Ok(format!("session ran; {} events lost", h.events_lost)),
        Some(h) => Err(format!("{} events lost during a 2 s test", h.events_lost)),
        None => Err("session started but its counters could not be read".to_string()),
    };

    let file_check = (|| {
        workload.as_ref().map_err(|e| format!("test workload failed: {e}"))?;
        let errs = errors_in("FileIo/");
        if !errs.is_empty() {
            return Err(format!("file event fields not found (layout changed?): {}", errs.join(", ")));
        }
        let me = iv.file_by_pid.get(&std::process::id());
        let (r, w) = me.map_or((0, 0), |a| (a.read_bytes, a.write_bytes));
        let file_total: u64 =
            iv.file_bytes.iter().filter(|(p, _)| p.ends_with(&name)).map(|(_, b)| *b).sum();
        // 8 MiB written by us; 8 MiB read by us, possibly plus a scan by antivirus. Writes beyond
        // 8 MiB would mean cache-flush (paging) traffic is being double counted.
        let ok = r >= want && w >= want && w <= want + want / 4 && file_total >= 2 * want && file_total <= 3 * want;
        let detail = format!(
            "test file: wrote {:.1} MB, read {:.1} MB; measured {:.1} MB written / {:.1} MB read by this process, {:.1} MB on the file",
            mb(want), mb(want), mb(w), mb(r), mb(file_total)
        );
        if ok { Ok(detail) } else { Err(detail) }
    })();

    let disk_check = (|| {
        let errs = errors_in("DiskIo/");
        if !errs.is_empty() {
            return Err(format!("disk event fields not found (layout changed?): {}", errs.join(", ")));
        }
        let n = iv.disk_attributed + iv.disk_fallback;
        if n == 0 {
            return Err("NOTRUN:no physical disk activity during the 2 s test".to_string());
        }
        if iv.disk_attributed == 0 {
            return Err(format!("{n} disk requests seen but none could be tied to a process"));
        }
        Ok(format!("{n} disk requests seen, {} attributed to a process", iv.disk_attributed))
    })();

    let dpc_check = (|| {
        let errs = errors_in("PerfInfo/");
        if !errs.is_empty() {
            return Err(format!("DPC/ISR event fields not found (layout changed?): {}", errs.join(", ")));
        }
        let dpcs: u64 = iv.dpc_by_routine.values().map(|a| a.count).sum();
        let isrs: u64 = iv.isr_by_routine.values().map(|a| a.count).sum();
        if iv.dur_negative > 0 {
            return Err(format!("{} negative durations: timestamp clock assumption is wrong", iv.dur_negative));
        }
        if dpcs == 0 {
            return Err("no DPC events in 2 s (opcodes changed?)".to_string());
        }
        let owned = {
            let table = crate::trace::driver_table();
            iv.dpc_by_routine.keys().filter(|r| crate::trace::owner(&table, **r) != "?").count()
        };
        Ok(format!(
            "{dpcs} DPCs and {isrs} ISRs timed; {owned} of {} DPC routines mapped to a driver; {} dropped as implausible",
            iv.dpc_by_routine.len(),
            iv.dur_implausible
        ))
    })();

    let to_cap = |id: &str, r: Result<String, String>| match r {
        Err(m) if m.starts_with("NOTRUN:") => not_run(id, profile, &m["NOTRUN:".len()..]),
        other => check(id, profile, other),
    };
    vec![
        to_cap("etw_kernel_session", session_check),
        to_cap("etw_disk_io", disk_check),
        to_cap("etw_file_io", file_check),
        to_cap("etw_dpc_isr", dpc_check),
    ]
}

/// Detects the OS, matches a profile, and runs every self-test that can run without a trace.
pub fn build() -> SystemProfile {
    let os = os_info();
    let (verification, profile) = find_profile(&os);
    let elevated = crate::pipe::is_elevated();

    let process_list = {
        let list = crate::procs::Sampler::new().processes();
        let me = std::process::id();
        if list.processes.iter().any(|p| p.pid == me) {
            Ok(format!("{} processes, including this one", list.processes.len()))
        } else {
            Err("own process missing from the process list".to_string())
        }
    };
    let counters = crate::mem::collect().and_then(|m| {
        if m.physical_total > 0 && m.physical_available <= m.physical_total {
            Ok(format!("{} MB total", m.physical_total / 1048576))
        } else {
            Err("implausible physical memory counters".to_string())
        }
    });

    // "Needs elevation" is a limit of how we were started, not a sign the build is unsupported,
    // so those two report NotRun rather than Failed when we aren't elevated.
    let mut caps = vec![
        check("process_list", &profile, process_list),
        check("memory_counters", &profile, counters),
    ];
    if elevated {
        caps.push(check("memory_lists", &profile, crate::mem::lists_selftest()));
        caps.push(check("handle_table_names", &profile, crate::files::selftest()));
    } else {
        caps.push(not_run("memory_lists", &profile, "needs an elevated collector"));
        caps.push(not_run("handle_table_names", &profile, "needs an elevated collector"));
    }
    if elevated {
        caps.extend(etw_checks(&profile));
    } else {
        for id in ETW_IDS {
            caps.push(not_run(id, &profile, "needs an elevated collector"));
        }
    }

    SystemProfile { os, verification, profile_title: profile.map(|p| p.title), capabilities: caps }
}

/// `collector profile`: prints this machine's OS info, which profile matched, and self-test
/// results. Run it on a new PC to see what a draft profile for it needs to cover.
pub fn run() {
    let os = os_info();
    let (_, profile) = find_profile(&os);
    let sp = build();
    println!("OS: {}.{} build {}.{} ({}, {}) {}", os.major, os.minor, os.build, os.ubr, os.display_version, os.edition, os.arch);
    println!("Elevated: {}", crate::pipe::is_elevated());
    match (&profile, sp.verification) {
        (Some(p), Verification::Exact) => println!("Profile: {} - verified on this exact build ({})", p.title, p.verified.date),
        (Some(p), _) => println!(
            "Profile: {} - same build number, but verified at patch level {} (this is {}). Self-tests decide.",
            p.title, p.verified.ubr, os.ubr
        ),
        (None, _) => println!("Profile: none for build {}. Everything below is unverified beyond the self-tests.", os.build),
    }
    println!("\nSelf-tests:");
    for c in &sp.capabilities {
        println!(
            "  {:<20} {:<7} {}{}",
            c.id,
            format!("{:?}", c.status).to_lowercase(),
            c.detail,
            if c.verified_in_profile { "  [verified in profile]" } else { "" }
        );
    }
    if let Some(p) = profile {
        println!("\nQuirks recorded for this build ({}):", p.id);
        for q in &p.quirks {
            println!("  - {q}");
        }
        println!("\nVerified on: {}", p.verified.machine);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_profiles_parse_and_are_unique() {
        let profiles = known_profiles();
        assert!(!profiles.is_empty());
        let mut ids: Vec<_> = profiles.iter().map(|p| p.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), profiles.len(), "duplicate profile ids");
        for p in &profiles {
            assert!(!p.verified_capabilities.is_empty(), "{} lists no verified capabilities", p.id);
        }
    }

    fn os(build: u32, ubr: u32) -> OsInfo {
        OsInfo { major: 10, minor: 0, build, ubr, arch: "x86_64".into(), display_version: String::new(), edition: String::new() }
    }

    #[test]
    fn matching_distinguishes_exact_patch_and_unknown_builds() {
        assert_eq!(find_profile(&os(19045, 7725)).0, Verification::Exact);
        assert_eq!(find_profile(&os(19045, 9999)).0, Verification::SameBuildOtherPatch);
        assert_eq!(find_profile(&os(22631, 1)).0, Verification::Untested);
    }
}
