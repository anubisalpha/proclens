//! Resolves kernel FileObject addresses (as seen in ETW FileIo events) to NT paths for files that
//! were opened before the trace started, by finding a handle that points at the object and asking
//! Windows for that handle's final path.
//!
//! Risks handled: querying some handles (e.g. synchronous pipes) is widely reported to block
//! forever (Microsoft's GetFileType/GetFinalPathNameByHandle pages don't warn about it), so every query
//! runs on a throwaway thread with a timeout; only disk-type handles are queried.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows::Win32::Storage::FileSystem::{
    GetFileType, GetFinalPathNameByHandleW, FILE_TYPE_DISK, GETFINALPATHNAMEBYHANDLE_FLAGS,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

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

/// Reads the whole system handle table (hundreds of thousands of entries on a busy machine).
fn read_table() -> Vec<HandleEntry> {
    let mut size = 8usize << 20;
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
            return Vec::new();
        }
        break buf;
    };
    let count = usize::from_ne_bytes(buf[..8].try_into().unwrap());
    let entry_size = std::mem::size_of::<HandleEntry>();
    let mut out = Vec::with_capacity(count.min(1 << 20));
    for i in 0..count {
        let off = 16 + i * entry_size;
        if off + entry_size > buf.len() {
            break;
        }
        out.push(unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const HandleEntry) });
    }
    out
}

fn find_candidates(wanted: &HashSet<usize>) -> Vec<Candidate> {
    let mut seen_objects = HashSet::new();
    read_table()
        .into_iter()
        .filter(|e| wanted.contains(&e.object) && seen_objects.insert(e.object))
        .map(|e| Candidate { object: e.object, pid: e.pid as u32, handle: e.handle })
        .collect()
}

/// Proves the handle-table layout and name lookup work on this build: opens our own executable,
/// finds that exact handle in the system table, and checks the path Windows gives back.
pub fn selftest() -> Result<String, String> {
    use std::os::windows::io::AsRawHandle;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = std::fs::File::open(&exe).map_err(|e| e.to_string())?;
    let (me, handle) = (std::process::id() as usize, file.as_raw_handle() as usize);
    let entry = read_table()
        .into_iter()
        .find(|e| e.pid == me && e.handle == handle)
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

/// Duplicates the handle into this process and returns its NT path, or None.
/// Runs the potentially blocking calls on a worker thread with a timeout.
fn name_of(c: &Candidate) -> Option<String> {
    let (pid, handle) = (c.pid, c.handle);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(name_blocking(pid, handle));
    });
    rx.recv_timeout(Duration::from_millis(500)).ok().flatten()
}

fn name_blocking(pid: u32, handle: usize) -> Option<String> {
    unsafe {
        let proc = OpenProcess(PROCESS_DUP_HANDLE, false, pid).ok()?;
        let mut dup = HANDLE::default();
        let ok = DuplicateHandle(
            proc,
            HANDLE(handle as *mut _),
            GetCurrentProcess(),
            &mut dup,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        );
        let _ = CloseHandle(proc);
        ok.ok()?;
        let name = if GetFileType(dup) == FILE_TYPE_DISK {
            let mut buf = [0u16; 1024];
            let n = GetFinalPathNameByHandleW(dup, &mut buf, VOLUME_NAME_NT) as usize;
            (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
        } else {
            None
        };
        let _ = CloseHandle(dup);
        name
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
