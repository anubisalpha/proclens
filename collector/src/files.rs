//! Resolves kernel FileObject addresses (as seen in ETW FileIo events) to NT paths for files that
//! were opened before the trace started, by finding a handle that points at the object and asking
//! Windows for that handle's final path.
//!
//! Risks handled: querying some handles (e.g. synchronous pipes) is widely reported to block
//! forever (Microsoft's GetFileType/GetFinalPathNameByHandle pages don't warn about it), so every query
//! runs on a throwaway thread with a timeout; only disk-type handles are queried. A query that
//! times out is abandoned by terminating its thread and closing the duplicated handle from here, so
//! a hang costs neither a thread nor an open handle (a leaked duplicate of a disk file would keep
//! that file open and block deleting it or dismounting its volume).
//!
//! Known limit: the (process, handle) pair comes from a snapshot of the handle table, so a handle
//! that was closed and reused before the lookup can name a different file. The result is a wrong
//! label for that one object, never a crash.

use std::collections::{HashMap, HashSet};
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows::Win32::Storage::FileSystem::{
    GetFileType, GetFinalPathNameByHandleW, FILE_TYPE_DISK, GETFINALPATHNAMEBYHANDLE_FLAGS,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, TerminateThread, PROCESS_DUP_HANDLE};

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut core::ffi::c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
}

const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;
const VOLUME_NAME_NT: GETFINALPATHNAMEBYHANDLE_FLAGS = GETFINALPATHNAMEBYHANDLE_FLAGS(2);
const QUERY_TIMEOUT: Duration = Duration::from_millis(500);

/// SYSTEM_HANDLE_TABLE_ENTRY_INFO_EX (undocumented layout, x64).
#[repr(C)]
#[derive(Clone, Copy)]
struct HandleEntry {
    object: usize,
    pid: usize,
    handle: usize,
    granted_access: u32,
    _creator_back_trace_index: u16,
    _object_type_index: u16,
    _attributes: u32,
    _reserved: u32,
}

/// A (process, handle value) pair that refers to a wanted object.
struct Candidate {
    object: usize,
    pid: u32,
    handle: usize,
}

/// Size of the last successful table read, so the next read usually succeeds first time.
static TABLE_SIZE_HINT: AtomicUsize = AtomicUsize::new(8 << 20);

/// Reads the whole system handle table (hundreds of thousands of entries on a busy machine) and
/// calls `visit` for each entry, without building a second copy of the table.
fn scan_table(mut visit: impl FnMut(&HandleEntry)) {
    let mut size = TABLE_SIZE_HINT.load(Ordering::Relaxed);
    let buf = loop {
        let mut buf = vec![0u8; size];
        let mut ret = 0u32;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_EXTENDED_HANDLE_INFORMATION,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                &mut ret,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            size = (ret as usize).max(size) * 2; // the table grows while we query
            continue;
        }
        if status != 0 {
            return;
        }
        TABLE_SIZE_HINT.store(size, Ordering::Relaxed);
        break buf;
    };
    let count = usize::from_ne_bytes(buf[..8].try_into().unwrap());
    let entry_size = std::mem::size_of::<HandleEntry>();
    for i in 0..count {
        let off = 16 + i * entry_size;
        if off + entry_size > buf.len() {
            break;
        }
        let entry = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const HandleEntry) };
        visit(&entry);
    }
}

fn find_candidates(wanted: &HashSet<usize>) -> Vec<Candidate> {
    let mut seen_objects = HashSet::new();
    let mut out = Vec::new();
    scan_table(|e| {
        if wanted.contains(&e.object) && seen_objects.insert(e.object) {
            out.push(Candidate { object: e.object, pid: e.pid as u32, handle: e.handle });
        }
    });
    out
}

/// Proves the handle-table layout and name lookup work on this build: opens our own executable,
/// finds that exact handle in the system table, and checks the path Windows gives back.
pub fn selftest() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = std::fs::File::open(&exe).map_err(|e| e.to_string())?;
    let (me, handle) = (std::process::id() as usize, file.as_raw_handle() as usize);
    let mut found = None;
    scan_table(|e| {
        if found.is_none() && e.pid == me && e.handle == handle {
            found = Some(*e);
        }
    });
    let entry = found
        .ok_or("own handle not found in the system handle table (layout changed, or table hidden)")?;
    if entry.object == 0 {
        return Err("kernel object address is hidden (collector not elevated?)".into());
    }
    let name = name_of(&Candidate { object: entry.object, pid: me as u32, handle })
        .ok_or("could not turn our own handle into a path")?;
    // NT form is \Device\HarddiskVolumeN\dir\file; compare everything after the drive letter.
    let plain = exe.to_string_lossy().replace(r"\\?\", "");
    let tail = plain.split_once(':').map(|(_, t)| t).unwrap_or(&plain).to_lowercase();
    if name.to_lowercase().ends_with(&tail) {
        Ok(format!("own handle resolved to {name}"))
    } else {
        Err(format!("own handle resolved to {name}, expected a path ending {tail}"))
    }
}

/// The worker is inside a call that may never return.
const IN_CALL: u8 = 0;
/// The worker's calls returned; it is only building the result now and must not be killed.
const DONE: u8 = 1;
/// The caller gave up and is terminating the worker.
const ABANDONED: u8 = 2;

/// Duplicates the handle into this process and returns its NT path, or None.
/// Runs the potentially blocking calls on a worker thread with a timeout.
fn name_of(c: &Candidate) -> Option<String> {
    name_of_with(c, query_path)
}

/// The calls on the duplicated handle that can hang. Returns raw UTF-16 so nothing is allocated
/// before the caller has been told the calls are over (see `name_blocking`).
type PathQuery = fn(HANDLE) -> ([u16; 1024], usize);

fn query_path(h: HANDLE) -> ([u16; 1024], usize) {
    let mut buf = [0u16; 1024];
    let n = unsafe {
        if GetFileType(h) == FILE_TYPE_DISK {
            GetFinalPathNameByHandleW(h, &mut buf, VOLUME_NAME_NT) as usize
        } else {
            0
        }
    };
    (buf, n)
}

fn name_of_with(c: &Candidate, query: PathQuery) -> Option<String> {
    let (pid, handle) = (c.pid, c.handle);
    let (tx, rx) = mpsc::channel();
    let dup = Arc::new(AtomicUsize::new(0));
    let phase = Arc::new(AtomicU8::new(IN_CALL));
    let worker = {
        let (dup, phase) = (dup.clone(), phase.clone());
        std::thread::spawn(move || {
            let _ = tx.send(name_blocking(pid, handle, query, &dup, &phase));
        })
    };
    match rx.recv_timeout(QUERY_TIMEOUT) {
        Ok(name) => name,
        Err(_) => {
            if phase.compare_exchange(IN_CALL, ABANDONED, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                // Stuck in a system call: stop it, then close what it was holding. Termination is
                // only used in this state, where the thread holds none of our locks.
                unsafe {
                    let _ = TerminateThread(HANDLE(worker.as_raw_handle()), 1);
                    let held = dup.swap(0, Ordering::SeqCst);
                    if held != 0 {
                        let _ = CloseHandle(HANDLE(held as *mut _));
                    }
                }
                None
            } else {
                // The calls returned just as we timed out; the worker is finishing normally.
                rx.recv().ok().flatten()
            }
        }
    }
}

fn name_blocking(pid: u32, handle: usize, query: PathQuery, dup: &AtomicUsize, phase: &AtomicU8) -> Option<String> {
    unsafe {
        let proc = OpenProcess(PROCESS_DUP_HANDLE, false, pid).ok()?;
        let mut copy = HANDLE::default();
        let ok = DuplicateHandle(
            proc,
            HANDLE(handle as *mut _),
            GetCurrentProcess(),
            &mut copy,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        );
        let _ = CloseHandle(proc);
        ok.ok()?;
        // Published so the caller can close it if this thread gets stuck below.
        dup.store(copy.0 as usize, Ordering::SeqCst);
        let (buf, n) = query(copy);
        // Past the calls that can hang. If the caller already gave up it is about to terminate
        // this thread: wait for that (parked, holding nothing) instead of racing it.
        if phase.compare_exchange(IN_CALL, DONE, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            loop {
                std::thread::park();
            }
        }
        let held = dup.swap(0, Ordering::SeqCst);
        if held != 0 {
            let _ = CloseHandle(HANDLE(held as *mut _));
        }
        (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
    }
}

/// Resolves as many of `wanted` as possible. Objects whose handles are gone (or protected) are
/// simply absent from the result.
pub fn resolve(wanted: &HashSet<usize>) -> HashMap<usize, String> {
    let mut out = HashMap::new();
    if wanted.is_empty() {
        return out;
    }
    for c in find_candidates(wanted) {
        if let Some(n) = name_of(&c) {
            out.insert(c.object, n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::GetProcessHandleCount;

    /// Tests that count the process's handles must not overlap with ones that open handles.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn handle_count() -> u32 {
        let mut n = 0u32;
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) }.unwrap();
        n
    }

    fn hang(_: HANDLE) -> ([u16; 1024], usize) {
        // park, not sleep: std's sleep creates a waitable-timer handle per call, which terminating
        // the thread would leak and make this test blame the code under test.
        loop {
            std::thread::park();
        }
    }

    fn own_exe_handle() -> (std::fs::File, Candidate) {
        let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        let c = Candidate { object: 0, pid: std::process::id(), handle: file.as_raw_handle() as usize };
        (file, c)
    }

    /// Stands in for the hang the module warns about (a query on a handle another thread is
    /// blocked on). The lookup must give up, stop the worker and close the handle it duplicated.
    #[test]
    fn a_query_that_hangs_is_abandoned_and_its_handle_closed() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (_file, c) = own_exe_handle();
        let before = handle_count();
        let started = Instant::now();
        let name = name_of_with(&c, hang);
        let took = started.elapsed();
        assert!(name.is_none());
        assert!(took >= QUERY_TIMEOUT - Duration::from_millis(50), "returned before the timeout: {took:?}");
        assert!(took < Duration::from_secs(3), "the lookup must give up, took {took:?}");
        std::thread::sleep(Duration::from_millis(300));
        let after = handle_count();
        // The worker thread's own handle is released once it is terminated; nothing may remain.
        assert!(after <= before, "leaked handles: {before} before, {after} after");
    }

    #[test]
    fn many_hangs_in_a_row_do_not_accumulate_handles_or_threads() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (_file, c) = own_exe_handle();
        let before = handle_count();
        for _ in 0..6 {
            assert!(name_of_with(&c, hang).is_none());
        }
        std::thread::sleep(Duration::from_millis(300));
        let after = handle_count();
        assert!(after <= before + 1, "leaked handles: {before} before, {after} after six hangs");
    }

    /// Pipes are not disk files: no path, and (on this Windows build at least) no hang either.
    #[test]
    fn a_pipe_handle_has_no_path() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
        unsafe { CreatePipe(&mut read, &mut write, None, 0) }.unwrap();
        let name = name_of(&Candidate { object: 0, pid: std::process::id(), handle: read.0 as usize });
        unsafe {
            let _ = CloseHandle(read);
            let _ = CloseHandle(write);
        }
        assert!(name.is_none());
    }

    #[test]
    fn a_normal_file_handle_still_resolves() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let exe = std::env::current_exe().unwrap();
        let file = std::fs::File::open(&exe).unwrap();
        let name = name_of(&Candidate {
            object: 0,
            pid: std::process::id(),
            handle: file.as_raw_handle() as usize,
        })
        .expect("a disk file resolves to an NT path");
        assert!(name.starts_with(r"\Device\"), "{name}");
        assert!(name.to_lowercase().ends_with(&exe.file_name().unwrap().to_string_lossy().to_lowercase()));
    }
}
