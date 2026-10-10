//! Memory reconciliation: where does physical RAM actually go?

use std::mem::size_of;

use proclens_proto::{MemoryLists, MemoryStats};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NOT_ALL_ASSIGNED, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut core::ffi::c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
}

const SYSTEM_MEMORY_LIST_INFORMATION_CLASS: u32 = 80;

/// Mirrors the (undocumented) SYSTEM_MEMORY_LIST_INFORMATION; all counts are in pages.
#[repr(C)]
#[derive(Default)]
struct SystemMemoryListInformation {
    zero_page_count: usize,
    free_page_count: usize,
    modified_page_count: usize,
    modified_no_write_page_count: usize,
    bad_page_count: usize,
    page_count_by_priority: [usize; 8],
    repurposed_pages_by_priority: [usize; 8],
    modified_page_count_page_file: usize,
}

/// Enables a privilege the process already holds. `Ok(false)` means it isn't held (not elevated).
pub fn enable_privilege(name: PCWSTR) -> Result<bool, String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(|e| format!("OpenProcessToken: {e}"))?;
        let mut luid = LUID::default();
        LookupPrivilegeValueW(None, name, &mut luid)
            .map_err(|e| format!("LookupPrivilegeValue: {e}"))?;
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let res = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        // AdjustTokenPrivileges "succeeds" even when the privilege isn't held; check last error.
        let not_assigned = GetLastError() == ERROR_NOT_ALL_ASSIGNED;
        let _ = CloseHandle(token);
        res.map_err(|e| format!("AdjustTokenPrivileges: {e}"))?;
        Ok(!not_assigned)
    }
}

/// Reads the memory picture. `lists` is `None` unless the process can use
/// `SeProfileSingleProcessPrivilege` (i.e. is elevated).
pub fn collect() -> Result<MemoryStats, String> {
    let mut pi = PERFORMANCE_INFORMATION::default();
    unsafe { GetPerformanceInfo(&mut pi, size_of::<PERFORMANCE_INFORMATION>() as u32) }
        .map_err(|e| format!("GetPerformanceInfo: {e}"))?;
    let page = pi.PageSize as u64;
    let bytes = |pages: usize| pages as u64 * page;

    Ok(MemoryStats {
        physical_total: bytes(pi.PhysicalTotal),
        physical_available: bytes(pi.PhysicalAvailable),
        system_cache: bytes(pi.SystemCache),
        kernel_nonpaged: bytes(pi.KernelNonpaged),
        kernel_paged: bytes(pi.KernelPaged),
        commit_total: bytes(pi.CommitTotal),
        commit_limit: bytes(pi.CommitLimit),
        handle_count: pi.HandleCount,
        process_count: pi.ProcessCount,
        thread_count: pi.ThreadCount,
        lists: collect_lists(&pi),
    })
}

fn collect_lists(pi: &PERFORMANCE_INFORMATION) -> Option<MemoryLists> {
    read_lists(pi).ok().map(|(l, _)| l)
}

/// Reads the page lists and refuses to return them unless they pass a consistency check. The
/// structure behind this call is undocumented, so a layout change on another Windows build would
/// otherwise produce plausible-looking nonsense. Invariant (held to within 1 MB on the verified
/// build): free + zeroed + standby pages == "available" pages, within a small tolerance.
fn read_lists(pi: &PERFORMANCE_INFORMATION) -> Result<(MemoryLists, String), String> {
    if !matches!(enable_privilege(w!("SeProfileSingleProcessPrivilege")), Ok(true)) {
        return Err("needs an elevated process (SeProfileSingleProcessPrivilege)".into());
    }
    let mut info = SystemMemoryListInformation::default();
    let mut ret = 0u32;
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_MEMORY_LIST_INFORMATION_CLASS,
            &mut info as *mut _ as *mut _,
            size_of::<SystemMemoryListInformation>() as u32,
            &mut ret,
        )
    };
    if status != 0 {
        return Err(format!("NtQuerySystemInformation(80) failed: NTSTATUS {status:#010x}"));
    }
    let standby: usize = info.page_count_by_priority.iter().sum();
    let free_zero = info.zero_page_count + info.free_page_count;
    let modified = info.modified_page_count + info.modified_no_write_page_count;

    let page = pi.PageSize as u64;
    let to_bytes = |pages: usize| pages as u64 * page;
    let (avail, summed) = (pi.PhysicalAvailable, free_zero + standby);
    // 0.5% of RAM or 32 MB, whichever is larger (the two reads happen microseconds apart).
    let tolerance = (pi.PhysicalTotal / 200).max(32 * 1024 * 1024 / pi.PageSize);
    if avail.abs_diff(summed) > tolerance || free_zero + standby + modified > pi.PhysicalTotal {
        return Err(format!(
            "page lists don't add up: available {} MB vs free+zero+standby {} MB",
            to_bytes(avail) / 1048576,
            to_bytes(summed) / 1048576
        ));
    }
    let in_use = pi
        .PhysicalTotal
        .saturating_sub(standby + free_zero + modified + info.bad_page_count);
    let detail = format!(
        "available {} MB vs free+zero+standby {} MB",
        to_bytes(avail) / 1048576,
        to_bytes(summed) / 1048576
    );
    Ok((
        MemoryLists {
            free_zero: to_bytes(free_zero),
            standby: to_bytes(standby),
            modified: to_bytes(modified),
            bad: to_bytes(info.bad_page_count),
            in_use: to_bytes(in_use),
        },
        detail,
    ))
}

/// Startup self-test for the page-list capability: `Ok(evidence)` or `Err(reason)`.
pub fn lists_selftest() -> Result<String, String> {
    let mut pi = PERFORMANCE_INFORMATION::default();
    unsafe { GetPerformanceInfo(&mut pi, size_of::<PERFORMANCE_INFORMATION>() as u32) }
        .map_err(|e| format!("GetPerformanceInfo: {e}"))?;
    read_lists(&pi).map(|(_, detail)| detail)
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

pub fn run() {
    let m = match collect() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    println!("== Memory ==");
    println!("Physical total      {:>10.0} MB", mb(m.physical_total));
    println!("Physical available  {:>10.0} MB", mb(m.physical_available));
    println!("System cache        {:>10.0} MB  (standby list + system working set; overlaps other rows)", mb(m.system_cache));
    println!("Kernel nonpaged     {:>10.0} MB", mb(m.kernel_nonpaged));
    println!("Kernel paged        {:>10.0} MB  (virtual; not all resident)", mb(m.kernel_paged));
    println!("Commit total/limit  {:>10.0} / {:.0} MB", mb(m.commit_total), mb(m.commit_limit));
    println!("Handles {}  Processes {}  Threads {}", m.handle_count, m.process_count, m.thread_count);
    match m.lists {
        None => println!("\nPage lists unavailable (needs an elevated process)."),
        Some(l) => {
            println!("\n== Page lists ==");
            println!("Free + zeroed       {:>10.0} MB", mb(l.free_zero));
            println!("Standby (cache)     {:>10.0} MB", mb(l.standby));
            println!("Modified            {:>10.0} MB", mb(l.modified));
            println!("Bad                 {:>10.0} MB", mb(l.bad));
            println!("In use (derived)    {:>10.0} MB", mb(l.in_use));
            println!(
                "Check: available {:.0} MB vs free+zero+standby {:.0} MB",
                mb(m.physical_available),
                mb(l.free_zero + l.standby)
            );
        }
    }
}
