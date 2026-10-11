//! Which TCP and UDP ports are in use, and by which program. Read by the app itself (no collector,
//! no elevation).
//!
//! Sources:
//! - `GetExtendedTcpTable` / `GetExtendedUdpTable` with the "owner PID" table classes: every TCP
//!   listener and connection and every bound UDP socket, IPv4 and IPv6, with the owning process id.
//!   The tables are parsed from raw bytes (the layouts are the documented `MIB_*ROW_OWNER_PID`
//!   structs) so the parsing can be tested on synthetic buffers.
//! - The Service Control Manager's list of running services (`EnumServicesStatusExW`), which says
//!   which services run inside each process. This is what turns "svchost.exe" into "Dnscache"; it
//!   needs no elevation. A process that hosts several services shows all of them: Windows does not say
//!   which one owns a given socket without the (elevated) owner-module tables.
//! - A ToolHelp process snapshot for program names (works for every process, unelevated) and
//!   `QueryFullProcessImageNameW` for the full path (denied for protected processes, which then show
//!   a name only).
//! - `netsh interface ipv4|ipv6 show excludedportrange`, for the ranges Windows keeps back so that
//!   nothing can bind them (Hyper-V, WinNAT, administrator-set). There is no simple documented API
//!   for these. Only the numbers and the `*` marker are read, so a localised netsh still parses.
//!
//! Remote addresses are shown in the view and nowhere else: the activity log never gets them (see
//! `docs/NETWORK_SCOPE.md`, section 3.6), and nothing here looks them up.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr};

use serde::Serialize;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Proto {
    Tcp,
    Udp,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub proto: Proto,
    pub v6: bool,
    pub local_addr: String,
    pub local_port: u16,
    /// None for listeners and UDP sockets, which have no peer.
    pub remote_addr: Option<String>,
    pub remote_port: Option<u16>,
    /// TCP state in words ("listen", "established", ...); None for UDP.
    pub state: Option<&'static str>,
    pub pid: u32,
}

/// The program behind one or more endpoints.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub pid: u32,
    /// None when Windows would not say (the process exited between the two readings).
    pub name: Option<String>,
    /// None when the process is protected or gone; the name can still be known.
    pub path: Option<String>,
    /// The Windows services running inside this process, by short name; empty for ordinary programs.
    pub services: Vec<Service>,
}

/// One Windows service, as the Service Control Manager names it.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Service {
    /// The short name ("Dnscache"), which is what `sc` and the registry use.
    pub name: String,
    /// The name Windows shows in services.msc ("DNS Client").
    pub display: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct PortsSnapshot {
    pub endpoints: Vec<Endpoint>,
    pub owners: Vec<Owner>,
    /// Set when the service list could not be read; owners then show no services.
    pub services_error: Option<String>,
}

/// A run of ports Windows will not let a program bind.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Excluded {
    pub proto: Proto,
    pub start: u16,
    pub end: u16,
    /// Marked `*` by netsh: set by an administrator rather than taken by Windows itself.
    pub administered: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ExcludedRanges {
    pub ranges: Vec<Excluded>,
    /// Set when netsh could not be run or read; the ranges are then incomplete.
    pub error: Option<String>,
}

/// MIB_TCP_STATE (iprtrmib.h).
pub fn tcp_state(code: u32) -> &'static str {
    match code {
        1 => "closed",
        2 => "listen",
        3 => "syn_sent",
        4 => "syn_received",
        5 => "established",
        6 => "fin_wait_1",
        7 => "fin_wait_2",
        8 => "close_wait",
        9 => "closing",
        10 => "last_ack",
        11 => "time_wait",
        12 => "delete_tcb",
        _ => "unknown",
    }
}

/// The port field of a row is a DWORD whose low 16 bits hold the port in network byte order.
pub fn port_of(raw: u32) -> u16 {
    u16::from_be((raw & 0xFFFF) as u16)
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn v4_at(buf: &[u8], off: usize) -> String {
    Ipv4Addr::new(buf[off], buf[off + 1], buf[off + 2], buf[off + 3]).to_string()
}

fn v6_at(buf: &[u8], off: usize) -> String {
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&buf[off..off + 16]);
    Ipv6Addr::from(octets).to_string()
}

/// Splits a table (a u32 row count, then the rows) into its rows. A count larger than the buffer
/// holds is clamped rather than trusted, so a torn read cannot index out of bounds.
fn rows(buf: &[u8], row_len: usize) -> impl Iterator<Item = &[u8]> {
    let declared = if buf.len() >= 4 { u32_at(buf, 0) as usize } else { 0 };
    let fits = buf.len().saturating_sub(4) / row_len;
    buf.get(4..).unwrap_or(&[]).chunks_exact(row_len).take(declared.min(fits))
}

/// `MIB_TCPTABLE_OWNER_PID`: state, local addr, local port, remote addr, remote port, pid (24 bytes).
pub fn parse_tcp4(buf: &[u8]) -> Vec<Endpoint> {
    rows(buf, 24).map(|r| tcp_endpoint(false, u32_at(r, 0), v4_at(r, 4), u32_at(r, 8), v4_at(r, 12), u32_at(r, 16), u32_at(r, 20))).collect()
}

/// `MIB_TCP6TABLE_OWNER_PID`: local addr(16), scope, port, remote addr(16), scope, port, state, pid (56 bytes).
pub fn parse_tcp6(buf: &[u8]) -> Vec<Endpoint> {
    rows(buf, 56).map(|r| tcp_endpoint(true, u32_at(r, 48), v6_at(r, 0), u32_at(r, 20), v6_at(r, 24), u32_at(r, 44), u32_at(r, 52))).collect()
}

fn tcp_endpoint(v6: bool, state: u32, laddr: String, lport: u32, raddr: String, rport: u32, pid: u32) -> Endpoint {
    let state = tcp_state(state);
    // A listener has no peer; Windows reports 0.0.0.0:0 for it.
    let has_peer = state != "listen" && state != "closed";
    Endpoint {
        proto: Proto::Tcp,
        v6,
        local_addr: laddr,
        local_port: port_of(lport),
        remote_addr: has_peer.then_some(raddr),
        remote_port: has_peer.then(|| port_of(rport)),
        state: Some(state),
        pid,
    }
}

/// `MIB_UDPTABLE_OWNER_PID`: local addr, local port, pid (12 bytes).
pub fn parse_udp4(buf: &[u8]) -> Vec<Endpoint> {
    rows(buf, 12).map(|r| udp_endpoint(false, v4_at(r, 0), u32_at(r, 4), u32_at(r, 8))).collect()
}

/// `MIB_UDP6TABLE_OWNER_PID`: local addr(16), scope, port, pid (28 bytes).
pub fn parse_udp6(buf: &[u8]) -> Vec<Endpoint> {
    rows(buf, 28).map(|r| udp_endpoint(true, v6_at(r, 0), u32_at(r, 20), u32_at(r, 24))).collect()
}

fn udp_endpoint(v6: bool, laddr: String, lport: u32, pid: u32) -> Endpoint {
    Endpoint {
        proto: Proto::Udp,
        v6,
        local_addr: laddr,
        local_port: port_of(lport),
        remote_addr: None,
        remote_port: None,
        state: None,
        pid,
    }
}

/// Reads `netsh ... show excludedportrange` output: lines of "start end" with an optional `*`.
/// Headings and the dashes line fail to parse as numbers and are skipped, in any language.
pub fn parse_excluded(proto: Proto, text: &str) -> Vec<Excluded> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let start: u16 = it.next()?.parse().ok()?;
            let end: u16 = it.next()?.parse().ok()?;
            let administered = it.next() == Some("*");
            (start <= end).then_some(Excluded { proto, start, end, administered })
        })
        .collect()
}

/// The distinct process ids the endpoints refer to, in order.
fn wanted_pids(endpoints: &[Endpoint]) -> Vec<u32> {
    let set: HashSet<u32> = endpoints.iter().map(|e| e.pid).collect();
    let mut v: Vec<u32> = set.into_iter().collect();
    v.sort_unstable();
    v
}

/// Groups (pid, service name, display name) rows by process, each process's services sorted by name
/// (ignoring case) without repeats. Rows for pid 0 (a service with no process) are dropped.
pub fn group_services(rows: impl IntoIterator<Item = (u32, String, String)>) -> HashMap<u32, Vec<Service>> {
    let mut out: HashMap<u32, Vec<Service>> = HashMap::new();
    for (pid, name, display) in rows {
        if pid != 0 {
            out.entry(pid).or_default().push(Service { name, display });
        }
    }
    for list in out.values_mut() {
        list.sort_by_key(|s| s.name.to_lowercase());
        list.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    }
    out
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::process::CommandExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Services::{
        CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW, SC_ENUM_PROCESS_INFO,
        SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_WIN32,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const ERROR_MORE_DATA: i32 = 234;

    /// Calls a table API until its buffer is big enough. The table can grow between the sizing call
    /// and the real one, so this retries a few times with some slack.
    fn fetch(call: impl Fn(Option<*mut c_void>, *mut u32) -> u32) -> Result<Vec<u8>, String> {
        let mut size: u32 = 0;
        for _ in 0..6 {
            let rc = call(None, &mut size);
            if rc != ERROR_INSUFFICIENT_BUFFER && rc != 0 {
                return Err(format!("the port table call failed with error {rc}"));
            }
            let mut buf = vec![0u8; size as usize + 4096];
            let mut len = buf.len() as u32;
            match call(Some(buf.as_mut_ptr() as *mut c_void), &mut len) {
                0 => {
                    buf.truncate(len as usize);
                    return Ok(buf);
                }
                ERROR_INSUFFICIENT_BUFFER => size = len,
                rc => return Err(format!("the port table call failed with error {rc}")),
            }
        }
        Err("the port table kept growing while it was being read".into())
    }

    fn tcp(family: u32) -> Result<Vec<u8>, String> {
        fetch(|p, size| unsafe { GetExtendedTcpTable(p, size, false, family, TCP_TABLE_OWNER_PID_ALL, 0) })
    }

    fn udp(family: u32) -> Result<Vec<u8>, String> {
        fetch(|p, size| unsafe { GetExtendedUdpTable(p, size, false, family, UDP_TABLE_OWNER_PID, 0) })
    }

    /// Program name by pid for every running process.
    fn process_names() -> HashMap<u32, String> {
        let mut out = HashMap::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
            let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            if Process32FirstW(snap, &mut entry).is_ok() {
                loop {
                    out.insert(entry.th32ProcessID, from_wide(&entry.szExeFile));
                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
        out
    }

    fn from_wide(buf: &[u16]) -> String {
        crate::network::from_wide(buf)
    }

    /// Full image path, or None when the process is protected, belongs to the system, or has gone.
    fn image_path(pid: u32) -> Option<String> {
        if pid == 0 || pid == 4 {
            return None;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = vec![0u16; 32768];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_FORMAT(0), PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(h);
            ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    /// The running services and the process each one lives in. The list is read in chunks: when the
    /// buffer is too small Windows returns what fits and asks to be called again.
    fn services() -> Result<HashMap<u32, Vec<Service>>, String> {
        let mut rows: Vec<(u32, String, String)> = Vec::new();
        unsafe {
            let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE)
                .map_err(|e| format!("could not open the Service Control Manager: {e}"))?;
            let mut buf = vec![0u8; 128 * 1024];
            let mut resume = 0u32;
            let result = loop {
                let (mut needed, mut returned) = (0u32, 0u32);
                let r = EnumServicesStatusExW(
                    scm,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    SERVICE_ACTIVE,
                    Some(&mut buf[..]),
                    &mut needed,
                    &mut returned,
                    Some(&mut resume),
                    PCWSTR::null(),
                );
                // The rows are valid even when more are waiting; their strings live in `buf` too.
                let size = std::mem::size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
                for i in 0..returned as usize {
                    let e = std::ptr::read_unaligned(buf.as_ptr().add(i * size) as *const ENUM_SERVICE_STATUS_PROCESSW);
                    let name = e.lpServiceName.to_string().unwrap_or_default();
                    let display = e.lpDisplayName.to_string().unwrap_or_default();
                    rows.push((e.ServiceStatusProcess.dwProcessId, name, display));
                }
                match r {
                    Ok(()) => break Ok(()),
                    Err(e) if e.code().0 == windows::core::HRESULT::from_win32(ERROR_MORE_DATA as u32).0 => {
                        if (needed as usize) > buf.len() {
                            buf.resize(needed as usize, 0);
                        }
                    }
                    Err(e) => break Err(format!("could not list the services: {e}")),
                }
            };
            let _ = CloseServiceHandle(scm);
            result?;
        }
        Ok(group_services(rows))
    }

    #[cfg(test)]
    pub fn services_for_test() -> Result<HashMap<u32, Vec<Service>>, String> {
        services()
    }

    pub fn snapshot() -> Result<PortsSnapshot, String> {
        let mut endpoints = Vec::new();
        endpoints.extend(parse_tcp4(&tcp(AF_INET)?));
        endpoints.extend(parse_tcp6(&tcp(AF_INET6)?));
        endpoints.extend(parse_udp4(&udp(AF_INET)?));
        endpoints.extend(parse_udp6(&udp(AF_INET6)?));

        let names = process_names();
        let (services, services_error) = match services() {
            Ok(s) => (s, None),
            Err(e) => (HashMap::new(), Some(e)),
        };
        let owners = wanted_pids(&endpoints)
            .into_iter()
            .map(|pid| Owner {
                pid,
                name: match pid {
                    0 => Some("System Idle Process".to_string()),
                    _ => names.get(&pid).cloned(),
                },
                path: image_path(pid),
                services: services.get(&pid).cloned().unwrap_or_default(),
            })
            .collect();
        Ok(PortsSnapshot { endpoints, owners, services_error })
    }

    fn netsh(proto: Proto, family: &str) -> Result<Vec<Excluded>, String> {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = std::path::Path::new(&root).join("System32").join("netsh.exe");
        let p = if proto == Proto::Tcp { "tcp" } else { "udp" };
        let out = std::process::Command::new(&exe)
            .args(["interface", family, "show", "excludedportrange", &format!("protocol={p}")])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("could not run netsh: {e}"))?;
        if !out.status.success() {
            return Err(format!("netsh exited with {}", out.status));
        }
        Ok(parse_excluded(proto, &String::from_utf8_lossy(&out.stdout)))
    }

    pub fn excluded() -> ExcludedRanges {
        let mut ranges: Vec<Excluded> = Vec::new();
        let mut error = None;
        // IPv4 and IPv6 hold the same ranges on Windows, so IPv4 is enough; IPv6 only fills in if IPv4 failed.
        for proto in [Proto::Tcp, Proto::Udp] {
            match netsh(proto, "ipv4").or_else(|_| netsh(proto, "ipv6")) {
                Ok(r) => ranges.extend(r),
                Err(e) => error = Some(e),
            }
        }
        ranges.sort_by_key(|r| (r.start, r.end));
        ExcludedRanges { ranges, error }
    }
}

#[cfg(windows)]
pub use win::{excluded, snapshot};

#[cfg(not(windows))]
pub fn snapshot() -> Result<PortsSnapshot, String> {
    Err("port information is only available on Windows".to_string())
}

#[cfg(not(windows))]
pub fn excluded() -> ExcludedRanges {
    ExcludedRanges { ranges: Vec::new(), error: Some("only available on Windows".to_string()) }
}

#[tauri::command]
pub async fn ports_snapshot() -> Result<PortsSnapshot, String> {
    tauri::async_runtime::spawn_blocking(snapshot).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn ports_excluded() -> Result<ExcludedRanges, String> {
    tauri::async_runtime::spawn_blocking(excluded).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// Port 8080 as Windows stores it: network byte order in the low 16 bits.
    fn raw_port(p: u16) -> u32 {
        u32::from(p.to_be())
    }

    fn table(count: u32, rows: &[Vec<u8>]) -> Vec<u8> {
        let mut b = le(count).to_vec();
        for r in rows {
            b.extend_from_slice(r);
        }
        b
    }

    fn tcp4_row(state: u32, laddr: [u8; 4], lport: u16, raddr: [u8; 4], rport: u16, pid: u32) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend(le(state));
        r.extend(laddr);
        r.extend(le(raw_port(lport)));
        r.extend(raddr);
        r.extend(le(raw_port(rport)));
        r.extend(le(pid));
        r
    }

    #[test]
    fn port_numbers_are_read_in_network_byte_order() {
        assert_eq!(port_of(raw_port(8080)), 8080);
        assert_eq!(port_of(raw_port(1)), 1);
        assert_eq!(port_of(raw_port(65535)), 65535);
        assert_eq!(port_of(0x0000_901F), 8080, "bytes 1F 90 on the wire");
        assert_eq!(port_of(0xFFFF_0000 | raw_port(443)), 443, "only the low 16 bits count");
    }

    #[test]
    fn tcp4_rows_become_listeners_and_connections() {
        let buf = table(
            2,
            &[
                tcp4_row(2, [0, 0, 0, 0], 8420, [0, 0, 0, 0], 0, 4321),
                tcp4_row(5, [192, 168, 1, 10], 51000, [140, 82, 112, 3], 443, 99),
            ],
        );
        let e = parse_tcp4(&buf);
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].local_port, e[0].state, e[0].pid), (8420, Some("listen"), 4321));
        assert_eq!(e[0].local_addr, "0.0.0.0");
        assert_eq!((e[0].remote_addr.clone(), e[0].remote_port), (None, None), "a listener has no peer");
        assert_eq!(e[1].state, Some("established"));
        assert_eq!(e[1].local_addr, "192.168.1.10");
        assert_eq!(e[1].remote_addr.as_deref(), Some("140.82.112.3"));
        assert_eq!(e[1].remote_port, Some(443));
    }

    #[test]
    fn tcp6_rows_are_read_at_the_right_offsets() {
        let mut row = Vec::new();
        row.extend(Ipv6Addr::LOCALHOST.octets());
        row.extend(le(0));
        row.extend(le(raw_port(5173)));
        row.extend([0u8; 16]);
        row.extend(le(0));
        row.extend(le(0));
        row.extend(le(2));
        row.extend(le(777));
        assert_eq!(row.len(), 56);
        let e = parse_tcp6(&table(1, &[row]));
        assert_eq!(e.len(), 1);
        assert!(e[0].v6);
        assert_eq!((e[0].local_addr.as_str(), e[0].local_port), ("::1", 5173));
        assert_eq!((e[0].state, e[0].pid), (Some("listen"), 777));
    }

    #[test]
    fn udp_rows_have_no_peer_and_no_state() {
        let mut r4 = Vec::new();
        r4.extend([127, 0, 0, 1]);
        r4.extend(le(raw_port(5353)));
        r4.extend(le(12));
        let e = parse_udp4(&table(1, &[r4]));
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].proto, e[0].local_port, e[0].pid, e[0].state), (Proto::Udp, 5353, 12, None));
        assert!(e[0].remote_addr.is_none());

        let mut r6 = Vec::new();
        r6.extend([0u8; 16]);
        r6.extend(le(0));
        r6.extend(le(raw_port(1900)));
        r6.extend(le(34));
        assert_eq!(r6.len(), 28);
        let e = parse_udp6(&table(1, &[r6]));
        assert_eq!((e[0].local_addr.as_str(), e[0].local_port, e[0].pid), ("::", 1900, 34));
    }

    #[test]
    fn a_count_larger_than_the_buffer_is_clamped_not_trusted() {
        let one = tcp4_row(2, [0; 4], 80, [0; 4], 0, 1);
        assert_eq!(parse_tcp4(&table(50, std::slice::from_ref(&one))).len(), 1, "count says 50, only one row is there");
        assert_eq!(parse_tcp4(&table(0, &[one])).len(), 0, "count says none");
        assert!(parse_tcp4(&[]).is_empty());
        assert!(parse_tcp4(&[1, 0]).is_empty());
        assert!(parse_udp6(&table(3, &[vec![0; 10]])).is_empty(), "a partial row is dropped");
    }

    #[test]
    fn unknown_tcp_states_do_not_panic() {
        assert_eq!(tcp_state(5), "established");
        assert_eq!(tcp_state(11), "time_wait");
        assert_eq!(tcp_state(0), "unknown");
        assert_eq!(tcp_state(99), "unknown");
    }

    #[test]
    fn netsh_output_parses_in_any_language() {
        let text = "\nProtocol tcp Port Exclusion Ranges\n\nStart Port    End Port      \n----------    --------      \n      2869        2869      \n     50000       50059     *\n\n* - Administered port exclusions.\n";
        let r = parse_excluded(Proto::Tcp, text);
        assert_eq!(
            r,
            vec![
                Excluded { proto: Proto::Tcp, start: 2869, end: 2869, administered: false },
                Excluded { proto: Proto::Tcp, start: 50000, end: 50059, administered: true },
            ]
        );
        // A localised heading is just more non-numeric lines.
        assert_eq!(parse_excluded(Proto::Udp, "Protokoll udp Portausschlussbereiche\n  5000  5010\n").len(), 1);
        assert!(parse_excluded(Proto::Tcp, "70000 70001\n9 3\n").is_empty(), "out of range or backwards");
    }

    #[test]
    fn wanted_pids_are_distinct_and_sorted() {
        let buf = table(3, &[tcp4_row(2, [0; 4], 80, [0; 4], 0, 5), tcp4_row(2, [0; 4], 81, [0; 4], 0, 5), tcp4_row(2, [0; 4], 80, [0; 4], 0, 3)]);
        let e = parse_tcp4(&buf);
        assert_eq!(wanted_pids(&e), vec![3, 5]);
    }

    #[cfg(windows)]
    #[test]
    fn the_real_tables_show_a_listener_this_test_opens() {
        let l4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let l6 = std::net::TcpListener::bind("[::1]:0");
        let u = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let (p4, pu) = (l4.local_addr().unwrap().port(), u.local_addr().unwrap().port());
        let me = std::process::id();

        let s = snapshot().expect("the owner tables read unelevated");
        let t = s.endpoints.iter().find(|e| e.proto == Proto::Tcp && e.local_port == p4).expect("our TCP listener is listed");
        assert_eq!((t.pid, t.state, t.local_addr.as_str()), (me, Some("listen"), "127.0.0.1"));
        let d = s.endpoints.iter().find(|e| e.proto == Proto::Udp && e.local_port == pu).expect("our UDP socket is listed");
        assert_eq!(d.pid, me);
        if let Ok(l6) = &l6 {
            let p6 = l6.local_addr().unwrap().port();
            assert!(s.endpoints.iter().any(|e| e.v6 && e.local_port == p6 && e.pid == me), "our IPv6 listener is listed");
        }

        let owner = s.owners.iter().find(|o| o.pid == me).expect("we are an owner");
        assert!(owner.name.as_deref().is_some_and(|n| n.to_lowercase().contains("proclens")), "{owner:?}");
        assert!(owner.path.is_some(), "we can read our own path");
    }

    #[cfg(windows)]
    #[test]
    fn excluded_ranges_read_without_error() {
        let r = excluded();
        assert!(r.error.is_none(), "{:?}", r.error);
        for x in &r.ranges {
            assert!(x.start <= x.end);
        }
    }

    #[test]
    fn services_are_grouped_by_process_sorted_and_without_repeats() {
        let g = group_services(vec![
            (100, "RpcSs".to_string(), "Remote Procedure Call (RPC)".to_string()),
            (100, "RpcEptMapper".to_string(), "RPC Endpoint Mapper".to_string()),
            (100, "rpcss".to_string(), "duplicate by case".to_string()),
            (200, "Dnscache".to_string(), "DNS Client".to_string()),
            (0, "NoProcess".to_string(), "dropped".to_string()),
        ]);
        assert_eq!(g.len(), 2, "pid 0 is dropped");
        let names: Vec<&str> = g[&100].iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["RpcEptMapper", "RpcSs"]);
        assert_eq!(g[&200], vec![Service { name: "Dnscache".into(), display: "DNS Client".into() }]);
    }

    #[cfg(windows)]
    #[test]
    fn the_real_service_list_reads_unelevated_and_knows_the_rpc_service() {
        let g = win::services_for_test().expect("the service list reads without elevation");
        let rpc = g.values().flatten().find(|s| s.name.eq_ignore_ascii_case("RpcSs")).expect("RpcSs always runs");
        assert!(!rpc.display.is_empty(), "{rpc:?}");
        let pid = g.iter().find(|(_, v)| v.iter().any(|s| s.name.eq_ignore_ascii_case("RpcSs"))).map(|(p, _)| *p).unwrap();
        assert!(pid > 4, "RpcSs runs in a svchost.exe, pid {pid}");
    }
}
