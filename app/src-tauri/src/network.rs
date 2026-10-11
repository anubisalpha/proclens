//! Network adapter counters, read by the app itself (no collector, no elevation).
//!
//! Source: `GetIfTable2`, the table of `MIB_IF_ROW2` rows that Windows keeps for every network
//! interface. The counters are 64-bit running totals (Microsoft's page does not say when they
//! restart), so rates come from differences between two readings (done in the UI, see
//! `src/network.ts`).
//!
//! The table also holds rows that are not network cards: "filter module" rows (the
//! `FilterInterface` flag), loopback and tunnel pseudo-interfaces. Those are dropped here. None of
//! the filter rows showed up on the verified PC, so that part is untested; the flag's meaning is
//! from Microsoft's MIB_IF_ROW2 page.

use std::sync::OnceLock;
use std::time::Instant;

use serde::Serialize;

/// What kind of link an adapter is, as far as Windows says. Bluetooth PAN adapters report the
/// Ethernet interface type, so the physical medium is checked first.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Medium {
    Ethernet,
    Wifi,
    Bluetooth,
    Mobile,
    Other,
}

#[derive(Serialize, Clone, Debug)]
pub struct Adapter {
    /// Stable identifier (the interface LUID); the interface index can change between boots.
    pub luid: u64,
    /// The name shown in Windows' network settings ("Ethernet 2").
    pub name: String,
    /// The driver's description ("Intel(R) 82579V Gigabit Network Connection").
    pub description: String,
    pub medium: Medium,
    /// A physical or virtual-bus network card, as opposed to a software adapter (VPN, virtual switch).
    pub hardware: bool,
    /// The adapters people mean by "my network" (see [`is_primary`]); the view and the log both use it.
    pub primary: bool,
    /// IF_OPER_STATUS: 1 up, 2 down, 3 testing, 4 unknown, 5 dormant, 6 not present, 7 lower layer down.
    pub oper_status: u32,
    /// None when Windows doesn't know.
    pub media_connected: Option<bool>,
    /// Link speed in bits per second (Microsoft's wording: "the speed in bits per second of the
    /// transmit link"); None when 0 or all-ones, which is how a disconnected adapter looked here.
    pub rx_speed_bps: Option<u64>,
    pub tx_speed_bps: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_discards: u64,
    pub tx_discards: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct NetworkSnapshot {
    /// Milliseconds on a monotonic clock, so two readings give a trustworthy interval.
    pub taken_ms: u64,
    pub adapters: Vec<Adapter>,
}

/// Result of the sanity check that runs when the Network view first opens.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SelfTest {
    pub ok: bool,
    pub detail: String,
}

// IANA ifType values (RFC 2863 / ipifcons.h).
const IF_TYPE_PPP: u32 = 23;
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_TYPE_IEEE80211: u32 = 71;
const IF_TYPE_TUNNEL: u32 = 131;
const IF_TYPE_WWANPP: u32 = 243;
const IF_TYPE_WWANPP2: u32 = 244;

// NDIS_PHYSICAL_MEDIUM values (ntddndis.h).
const MEDIUM_WIRELESS_LAN: u32 = 1;
const MEDIUM_WIRELESS_WAN: u32 = 8;
const MEDIUM_NATIVE_802_11: u32 = 9;
const MEDIUM_BLUETOOTH: u32 = 10;

/// Rows that are not adapters in their own right: lightweight-filter copies, loopback and tunnels.
pub fn is_real_adapter(filter_interface: bool, if_type: u32) -> bool {
    !filter_interface && if_type != IF_TYPE_SOFTWARE_LOOPBACK && if_type != IF_TYPE_TUNNEL
}

/// Real network cards plus Wi-Fi, Bluetooth and mobile links. Windows also lists software adapters
/// (WAN Miniport entries, the kernel debugger, virtual switches); those are not "primary". Bluetooth
/// counts because its PAN adapter is a software adapter even though the radio is hardware.
pub fn is_primary(hardware: bool, medium: Medium) -> bool {
    hardware || matches!(medium, Medium::Wifi | Medium::Bluetooth | Medium::Mobile)
}

pub fn medium_of(physical_medium: u32, if_type: u32) -> Medium {
    match physical_medium {
        MEDIUM_BLUETOOTH => return Medium::Bluetooth,
        MEDIUM_NATIVE_802_11 | MEDIUM_WIRELESS_LAN => return Medium::Wifi,
        MEDIUM_WIRELESS_WAN => return Medium::Mobile,
        _ => {}
    }
    match if_type {
        IF_TYPE_IEEE80211 => Medium::Wifi,
        IF_TYPE_WWANPP | IF_TYPE_WWANPP2 => Medium::Mobile,
        IF_TYPE_PPP => Medium::Other,
        6 => Medium::Ethernet, // IF_TYPE_ETHERNET_CSMACD
        _ => Medium::Other,
    }
}

/// The docs define no "unknown" speed. 0 is what a disconnected adapter reported on the verified PC
/// and all-ones is the usual "not set" value, so both are shown as unknown.
pub fn speed_or_none(bps: u64) -> Option<u64> {
    (bps != 0 && bps != u64::MAX).then_some(bps)
}

/// Turns the wide, NUL-terminated fixed buffer in a row into a String.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn clock_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Checks a reading for the things that would make the numbers meaningless. `second` is a later
/// reading than `first`; the counters are running totals, so none may go backwards.
pub fn check_readings(first: &NetworkSnapshot, second: &NetworkSnapshot) -> SelfTest {
    if first.adapters.is_empty() {
        return SelfTest { ok: false, detail: "Windows listed no network adapters".into() };
    }
    if first.adapters.iter().any(|a| a.name.is_empty()) {
        return SelfTest { ok: false, detail: "an adapter came back with no name (table layout changed?)".into() };
    }
    for a in &first.adapters {
        if let Some(b) = second.adapters.iter().find(|b| b.luid == a.luid) {
            let backwards = (b.rx_bytes < a.rx_bytes) || (b.tx_bytes < a.tx_bytes);
            if backwards {
                return SelfTest { ok: false, detail: format!("{}: the byte counters went backwards between two readings", a.name) };
            }
        }
    }
    let hardware = first.adapters.iter().filter(|a| a.hardware).count();
    SelfTest {
        ok: true,
        detail: format!(
            "{} adapters ({} hardware); counters only ever increased between two readings",
            first.adapters.len(),
            hardware
        ),
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};

    /// Reads the interface table. Fails only if Windows refuses the call.
    pub fn snapshot() -> Result<NetworkSnapshot, String> {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        let rc = unsafe { GetIfTable2(&mut table) };
        if rc.0 != 0 || table.is_null() {
            return Err(format!("GetIfTable2 failed with error {}", rc.0));
        }
        let mut adapters = Vec::new();
        unsafe {
            let count = (*table).NumEntries as usize;
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
            for row in rows {
                // Bit 0 HardwareInterface, bit 1 FilterInterface (see MIB_IF_ROW2's bitfield).
                let flags = row.InterfaceAndOperStatusFlags._bitfield;
                let (hardware, filter) = (flags & 0b01 != 0, flags & 0b10 != 0);
                if !is_real_adapter(filter, row.Type) {
                    continue;
                }
                let medium = medium_of(row.PhysicalMediumType.0 as u32, row.Type);
                adapters.push(Adapter {
                    luid: row.InterfaceLuid.Value,
                    name: from_wide(&row.Alias),
                    description: from_wide(&row.Description),
                    medium,
                    hardware,
                    primary: is_primary(hardware, medium),
                    oper_status: row.OperStatus.0 as u32,
                    media_connected: match row.MediaConnectState.0 {
                        1 => Some(true),
                        2 => Some(false),
                        _ => None,
                    },
                    rx_speed_bps: speed_or_none(row.ReceiveLinkSpeed),
                    tx_speed_bps: speed_or_none(row.TransmitLinkSpeed),
                    rx_bytes: row.InOctets,
                    tx_bytes: row.OutOctets,
                    rx_packets: row.InUcastPkts.saturating_add(row.InNUcastPkts),
                    tx_packets: row.OutUcastPkts.saturating_add(row.OutNUcastPkts),
                    rx_errors: row.InErrors,
                    tx_errors: row.OutErrors,
                    rx_discards: row.InDiscards,
                    tx_discards: row.OutDiscards,
                });
            }
            FreeMibTable(table as *const _);
        }
        Ok(NetworkSnapshot { taken_ms: clock_ms(), adapters })
    }
}

#[cfg(windows)]
pub use win::snapshot;

#[cfg(not(windows))]
pub fn snapshot() -> Result<NetworkSnapshot, String> {
    Err("network counters are only available on Windows".to_string())
}

/// Two readings a moment apart, then `check_readings`.
pub fn selftest() -> SelfTest {
    let first = match snapshot() {
        Ok(s) => s,
        Err(e) => return SelfTest { ok: false, detail: e },
    };
    std::thread::sleep(std::time::Duration::from_millis(150));
    match snapshot() {
        Ok(second) => check_readings(&first, &second),
        Err(e) => SelfTest { ok: false, detail: e },
    }
}

#[tauri::command]
pub async fn network_snapshot() -> Result<NetworkSnapshot, String> {
    tauri::async_runtime::spawn_blocking(snapshot).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn network_selftest() -> Result<SelfTest, String> {
    tauri::async_runtime::spawn_blocking(selftest).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            rx_speed_bps: Some(1_000_000_000),
            tx_speed_bps: Some(1_000_000_000),
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

    fn snap(adapters: Vec<Adapter>) -> NetworkSnapshot {
        NetworkSnapshot { taken_ms: 0, adapters }
    }

    #[test]
    fn filter_loopback_and_tunnel_rows_are_not_adapters() {
        assert!(is_real_adapter(false, 6));
        assert!(!is_real_adapter(true, 6), "a lightweight-filter copy of an adapter");
        assert!(!is_real_adapter(false, IF_TYPE_SOFTWARE_LOOPBACK));
        assert!(!is_real_adapter(false, IF_TYPE_TUNNEL));
        assert!(is_real_adapter(false, IF_TYPE_IEEE80211));
    }

    #[test]
    fn bluetooth_pan_is_recognised_even_though_it_reports_the_ethernet_type() {
        assert_eq!(medium_of(MEDIUM_BLUETOOTH, 6), Medium::Bluetooth);
        assert_eq!(medium_of(14, 6), Medium::Ethernet); // 802.3
        assert_eq!(medium_of(MEDIUM_NATIVE_802_11, 6), Medium::Wifi);
        assert_eq!(medium_of(0, IF_TYPE_IEEE80211), Medium::Wifi);
        assert_eq!(medium_of(0, IF_TYPE_WWANPP), Medium::Mobile);
        assert_eq!(medium_of(0, 53), Medium::Other); // propVirtual
    }

    #[test]
    fn primary_adapters_are_cards_and_wireless_links_but_not_software_adapters() {
        assert!(is_primary(true, Medium::Ethernet));
        assert!(is_primary(false, Medium::Bluetooth), "the PAN adapter is software, the radio is not");
        assert!(is_primary(false, Medium::Wifi));
        assert!(is_primary(false, Medium::Mobile));
        assert!(!is_primary(false, Medium::Ethernet), "WAN Miniport and the kernel debugger report Ethernet");
        assert!(!is_primary(false, Medium::Other));
    }

    #[test]
    fn unknown_link_speeds_are_none() {
        assert_eq!(speed_or_none(0), None);
        assert_eq!(speed_or_none(u64::MAX), None);
        assert_eq!(speed_or_none(1_000_000_000), Some(1_000_000_000));
    }

    #[test]
    fn wide_buffers_stop_at_the_first_nul() {
        let mut buf = [0u16; 8];
        for (i, c) in "Ethernet".encode_utf16().take(4).enumerate() {
            buf[i] = c;
        }
        assert_eq!(from_wide(&buf), "Ethe");
        assert_eq!(from_wide(&[72, 105]), "Hi", "no NUL at all");
        assert_eq!(from_wide(&[]), "");
    }

    #[test]
    fn self_test_passes_for_counters_that_only_grow() {
        let a = snap(vec![adapter(1, "Ethernet", 100, 50), adapter(2, "Wi-Fi", 0, 0)]);
        let b = snap(vec![adapter(1, "Ethernet", 150, 50), adapter(2, "Wi-Fi", 0, 10)]);
        let t = check_readings(&a, &b);
        assert!(t.ok, "{}", t.detail);
        assert!(t.detail.contains("2 adapters"));
    }

    #[test]
    fn self_test_fails_for_backwards_counters_or_nothing_listed() {
        let a = snap(vec![adapter(1, "Ethernet", 100, 50)]);
        let b = snap(vec![adapter(1, "Ethernet", 99, 50)]);
        let t = check_readings(&a, &b);
        assert!(!t.ok && t.detail.contains("backwards"), "{}", t.detail);
        assert!(!check_readings(&snap(vec![]), &snap(vec![])).ok);
        assert!(!check_readings(&snap(vec![adapter(1, "", 0, 0)]), &snap(vec![])).ok);
    }

    #[test]
    fn an_adapter_that_disappears_between_readings_is_not_a_failure() {
        let a = snap(vec![adapter(1, "Ethernet", 100, 50), adapter(2, "USB", 5, 5)]);
        let b = snap(vec![adapter(1, "Ethernet", 120, 60)]);
        assert!(check_readings(&a, &b).ok);
    }

    #[cfg(windows)]
    #[test]
    fn the_real_table_reads_and_passes_its_own_checks() {
        let s = snapshot().expect("GetIfTable2 works unelevated");
        assert!(!s.adapters.is_empty(), "every Windows PC has at least one non-loopback interface");
        for a in &s.adapters {
            assert!(!a.name.is_empty());
        }
        let t = selftest();
        assert!(t.ok, "{}", t.detail);
    }
}
