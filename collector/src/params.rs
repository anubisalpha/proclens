//! Build-specific ETW parameters: event opcodes, provider flags and a few numeric facts about
//! how this Windows build reports file and disk activity.
//!
//! They live in the profile JSON (`profiles/*.json`, section `etw`) rather than in code because
//! they are undocumented and have differed between Windows builds. Supporting a new build means
//! adding a profile, not editing the trace code. Event *field names* (IrpPtr, FileObject, ...)
//! are still fixed in `trace.rs`; the startup self-tests catch a build where those differ.
//!
//! Parsing is strict (`deny_unknown_fields`, no defaults for values) and [`EtwParams::validate`]
//! rejects contradictory tables, so a typo in a profile fails at startup or in `cargo test`
//! instead of silently dropping events.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};

/// A `u32` written in JSON either as a number or as a string like `"0x02000000"` (flag values
/// are far more readable in hex).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hex(pub u32);

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Num(u32),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Num(n) => Ok(Hex(n)),
            Raw::Text(s) => {
                let t = s.trim();
                let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    Some(hex) => u32::from_str_radix(&hex.replace('_', ""), 16),
                    None => t.parse::<u32>(),
                };
                parsed.map(Hex).map_err(|e| serde::de::Error::custom(format!("bad number {s:?}: {e}")))
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EtwParams {
    /// Header timestamps are raw QPC ticks (needs the vendored ferrisetw patch). The duration
    /// maths assumes this, so a profile saying `false` is rejected rather than half-supported.
    pub raw_timestamps: bool,
    /// Free-text facts about this build's events, for people reading the profile.
    #[serde(default)]
    #[allow(dead_code)] // read by people, not code; kept in the struct so deny_unknown_fields accepts it
    pub notes: Vec<String>,
    pub thread: ThreadParams,
    pub file: FileParams,
    pub disk: DiskParams,
    pub perf: PerfParams,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadParams {
    /// Thread Start and DCStart (rundown of threads that already existed): both give tid -> pid.
    pub start_opcodes: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileParams {
    /// Kernel flags OR-ed to enable file events. Named so the JSON says what each one is.
    pub flags: BTreeMap<String, Hex>,
    /// Events that carry FileObject -> name (never observed live on the verified build).
    pub name_opcodes: Vec<u8>,
    pub create_opcode: u8,
    pub close_opcode: u8,
    pub read_opcode: u8,
    pub write_opcode: u8,
    /// Completion event; its ExtraInfo is the number of bytes actually transferred.
    pub operation_end_opcode: u8,
    /// `IoFlags` bits that mark paging I/O (cache flushes, page-ins).
    pub paging_io_flags: Hex,
    /// NTSTATUS of "read past end of file", which is not a failed request.
    pub end_of_file_ntstatus: Hex,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskParams {
    pub read_opcode: u8,
    pub write_opcode: u8,
    // `HighResResponseTime` is not a parameter: Microsoft documents it as KeQueryPerformanceCounter
    // ticks, so it is converted with the machine's actual counter frequency at runtime.
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerfParams {
    pub flags: BTreeMap<String, Hex>,
    pub dpc_opcodes: Vec<u8>,
    pub isr_opcodes: Vec<u8>,
}

fn or_flags(flags: &BTreeMap<String, Hex>) -> u32 {
    flags.values().fold(0, |acc, h| acc | h.0)
}

impl EtwParams {
    /// Kernel flag mask for the file provider.
    pub fn file_flags(&self) -> u32 {
        or_flags(&self.file.flags)
    }

    /// Kernel flag mask for the DPC/ISR (PerfInfo) provider.
    pub fn perf_flags(&self) -> u32 {
        or_flags(&self.perf.flags)
    }

    pub fn thread_wants(&self, op: u8) -> bool {
        self.thread.start_opcodes.contains(&op)
    }

    pub fn file_wants(&self, op: u8) -> bool {
        let f = &self.file;
        f.name_opcodes.contains(&op)
            || [f.create_opcode, f.close_opcode, f.read_opcode, f.write_opcode, f.operation_end_opcode].contains(&op)
    }

    pub fn disk_wants(&self, op: u8) -> bool {
        op == self.disk.read_opcode || op == self.disk.write_opcode
    }

    pub fn perf_wants(&self, op: u8) -> bool {
        self.is_dpc(op) || self.is_isr(op)
    }

    pub fn is_dpc(&self, op: u8) -> bool {
        self.perf.dpc_opcodes.contains(&op)
    }

    pub fn is_isr(&self, op: u8) -> bool {
        self.perf.isr_opcodes.contains(&op)
    }

    pub fn is_paging(&self, io_flags: u32) -> bool {
        io_flags & self.file.paging_io_flags.0 != 0
    }

    /// Rejects tables that can't be right. Opcode groups must not overlap, because the handler
    /// decides what an event is by testing these sets in turn.
    pub fn validate(&self) -> Result<(), String> {
        if !self.raw_timestamps {
            return Err("raw_timestamps must be true: duration maths needs QPC header timestamps".into());
        }
        let f = &self.file;
        let single = [f.create_opcode, f.close_opcode, f.read_opcode, f.write_opcode, f.operation_end_opcode];
        let mut seen: Vec<u8> = f.name_opcodes.clone();
        seen.extend(single);
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1]) {
            return Err(format!("file opcodes overlap: {seen:?}"));
        }
        if self.disk.read_opcode == self.disk.write_opcode {
            return Err("disk read and write opcodes are the same".into());
        }
        if self.thread.start_opcodes.is_empty() {
            return Err("thread.start_opcodes is empty".into());
        }
        if self.perf.dpc_opcodes.is_empty() || self.perf.isr_opcodes.is_empty() {
            return Err("perf needs at least one DPC and one ISR opcode".into());
        }
        if self.perf.dpc_opcodes.iter().any(|o| self.perf.isr_opcodes.contains(o)) {
            return Err("an opcode is listed as both DPC and ISR".into());
        }
        if self.file_flags() == 0 || self.perf_flags() == 0 {
            return Err("file and perf provider flags must be non-empty".into());
        }
        if f.paging_io_flags.0 == 0 {
            return Err("file.paging_io_flags is zero, so paging I/O would never be recognised".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete, valid parameter set to mutate in the negative tests.
    const GOOD: &str = r#"{
        "raw_timestamps": true,
        "thread": { "start_opcodes": [1, 3] },
        "file": {
            "flags": { "FILE_IO": "0x02000000", "FILE_IO_INIT": "0x04000000", "DISK_FILE_IO": "0x200" },
            "name_opcodes": [0, 32, 35, 36],
            "create_opcode": 64, "close_opcode": 66, "read_opcode": 67, "write_opcode": 68,
            "operation_end_opcode": 76,
            "paging_io_flags": "0x42",
            "end_of_file_ntstatus": "0xC0000011"
        },
        "disk": { "read_opcode": 10, "write_opcode": 11 },
        "perf": { "flags": { "DPC": "0x20", "INTERRUPT": "0x40" }, "dpc_opcodes": [66, 68, 69], "isr_opcodes": [50, 67] }
    }"#;

    fn good() -> EtwParams {
        serde_json::from_str(GOOD).unwrap()
    }

    #[test]
    fn hex_accepts_strings_underscores_and_plain_numbers() {
        let h = |s: &str| serde_json::from_str::<Hex>(s).unwrap().0;
        assert_eq!(h(r#""0x02000000""#), 0x0200_0000);
        assert_eq!(h(r#""0XC000_0011""#), 0xC000_0011);
        assert_eq!(h("66"), 66);
        assert_eq!(h(r#""66""#), 66);
        assert!(serde_json::from_str::<Hex>(r#""0xZZ""#).is_err());
        assert!(serde_json::from_str::<Hex>(r#""-1""#).is_err());
    }

    #[test]
    fn derived_masks_and_predicates() {
        let p = good();
        p.validate().unwrap();
        assert_eq!(p.file_flags(), 0x0600_0200);
        assert_eq!(p.perf_flags(), 0x60);
        assert!(p.thread_wants(3) && !p.thread_wants(2));
        for op in [0, 32, 35, 36, 64, 66, 67, 68, 76] {
            assert!(p.file_wants(op), "file op {op}");
        }
        assert!(!p.file_wants(65));
        assert!(p.disk_wants(10) && p.disk_wants(11) && !p.disk_wants(14));
        assert!(p.perf_wants(50) && p.perf_wants(69) && !p.perf_wants(46));
        assert!(p.is_isr(50) && p.is_isr(67) && !p.is_isr(68));
        assert!(p.is_dpc(68) && !p.is_dpc(67));
        assert!(p.is_paging(0x2) && p.is_paging(0x40) && p.is_paging(0x42) && !p.is_paging(0x1));
    }

    #[test]
    fn unknown_or_misspelled_keys_are_rejected() {
        let typo = GOOD.replace("dpc_opcodes", "dpc_opcode");
        assert!(serde_json::from_str::<EtwParams>(&typo).is_err());
        let extra = GOOD.replace(r#""raw_timestamps": true,"#, r#""raw_timestamps": true, "bogus": 1,"#);
        assert!(serde_json::from_str::<EtwParams>(&extra).is_err());
    }

    #[test]
    fn contradictory_tables_fail_validation() {
        let mut p = good();
        p.file.close_opcode = 64; // same as create
        assert!(p.validate().unwrap_err().contains("overlap"));

        let mut p = good();
        p.file.name_opcodes.push(67); // also the read opcode
        assert!(p.validate().unwrap_err().contains("overlap"));

        let mut p = good();
        p.perf.isr_opcodes.push(68); // 68 is a DPC
        assert!(p.validate().is_err());

        let mut p = good();
        p.disk.write_opcode = p.disk.read_opcode;
        assert!(p.validate().is_err());

        let mut p = good();
        p.raw_timestamps = false;
        assert!(p.validate().is_err());

        let mut p = good();
        p.file.paging_io_flags = Hex(0);
        assert!(p.validate().is_err());
    }
}
