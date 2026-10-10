//! ProcLens wire protocol.
//!
//! The elevated collector and the unelevated UI talk over a Windows named pipe. Every message is a
//! frame: a `u32` little-endian length followed by that many bytes of JSON (see [`frame`]).
//! JSON keeps the spike debuggable; swap the codec in `frame` if volume ever demands it.
//!
//! The collector is **read-only** with respect to the system: nothing here can change another
//! process. The only mutating request is [`Request::Shutdown`], which stops the collector itself
//! (the unelevated UI could not otherwise stop an elevated process it launched).

use serde::{Deserialize, Serialize};

pub mod frame;
#[cfg(windows)]
pub mod client;

/// Bumped on incompatible changes. The pipe name carries the major version too, so an old UI and
/// a new collector simply don't find each other instead of misparsing frames.
pub const PROTOCOL_VERSION: u32 = 1;
pub const PIPE_NAME: &str = r"\\.\pipe\proclens-collector-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Memory,
    Processes,
    /// What this Windows build is and which features were verified to work on it. Snapshot-only
    /// in practice: it doesn't change while the collector runs.
    Profile,
    /// Disk, file and driver (DPC/ISR) activity from a kernel trace. The collector only runs the
    /// trace while at least one client is subscribed, and pushes one snapshot per
    /// [`ACTIVITY_INTERVAL_MS`] regardless of the requested interval.
    Activity,
}

/// Cadence of [`Topic::Activity`] snapshots.
pub const ACTIVITY_INTERVAL_MS: u32 = 2000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Must be the first message on a connection.
    Hello { protocol_version: u32, client: String },
    /// Push `topic` every `interval_ms` (the collector clamps this to 250..=60000).
    Subscribe { topic: Topic, interval_ms: u32 },
    Unsubscribe { topic: Topic },
    /// One-off read of a topic.
    Snapshot { topic: Topic },
    /// Ask the collector process to exit.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    HelloAck {
        protocol_version: u32,
        collector_version: String,
        collector_pid: u32,
        /// Self-reported; the client must not rely on it (it verifies the server process itself).
        elevated: bool,
    },
    Memory(MemoryStats),
    Processes(ProcessList),
    Profile(SystemProfile),
    Activity(ActivitySnapshot),
    Subscribed { topic: Topic, interval_ms: u32 },
    Unsubscribed { topic: Topic },
    Error { message: String },
    Bye,
}

/// All sizes are bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryStats {
    pub physical_total: u64,
    pub physical_available: u64,
    /// Microsoft: "the size of the standby list plus the system working set". It overlaps the
    /// page lists below, so it is a view, not a partition.
    pub system_cache: u64,
    pub kernel_nonpaged: u64,
    /// Virtual size of paged pool; not all of it is resident.
    pub kernel_paged: u64,
    pub commit_total: u64,
    pub commit_limit: u64,
    pub handle_count: u32,
    pub process_count: u32,
    pub thread_count: u32,
    /// Only available when the collector is elevated.
    pub lists: Option<MemoryLists>,
}

/// The physical page lists; `in_use` is derived as total - (free_zero + standby + modified + bad).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryLists {
    pub free_zero: u64,
    pub standby: u64,
    pub modified: u64,
    pub bad: u64,
    pub in_use: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub working_set: u64,
    pub virtual_size: u64,
    /// Percent of one core since the previous sample; 0 on the first sample.
    pub cpu_percent: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessList {
    pub processes: Vec<ProcessInfo>,
    pub total_working_set: u64,
}

/// What happened during one interval (a *delta*, not a running total). Divide by `interval_ms`
/// for rates. Lists are trimmed to the most active entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivitySnapshot {
    pub interval_ms: u32,
    pub logical_cpus: u32,
    pub processes: Vec<ProcessActivity>,
    pub files: Vec<FileActivity>,
    pub drivers: Vec<DriverActivity>,
    pub health: ActivityHealth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessActivity {
    pub pid: u32,
    pub name: String,
    /// Physical disk traffic issued by this process (after the file cache).
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub disk_ops: u64,
    /// Mean response time of this process's disk requests; `None` if it had none.
    pub disk_latency_us: Option<f64>,
    /// File-level traffic by this process (before the file cache), in bytes actually transferred.
    pub file_read_bytes: u64,
    pub file_write_bytes: u64,
    pub file_ops: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileActivity {
    /// Drive-letter path where the volume is known, otherwise the raw NT path. A placeholder text
    /// is used for traffic whose file could not be identified.
    pub path: String,
    pub bytes: u64,
}

/// CPU time spent in a driver's interrupt handlers, which Task Manager folds into "System".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriverActivity {
    pub name: String,
    pub dpc_calls: u64,
    pub dpc_time_us: f64,
    pub dpc_max_us: f64,
    pub isr_calls: u64,
    pub isr_time_us: f64,
    pub isr_max_us: f64,
}

/// Evidence about how complete this interval's data is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityHealth {
    /// Cumulative since the trace started; non-zero means some numbers are too low.
    pub events_lost: u32,
    pub buffers_lost: u32,
    /// File requests that failed (transferred 0 bytes).
    pub io_failed: u64,
    /// DPC/ISR events discarded for an implausible duration.
    pub durations_dropped: u64,
    /// File bytes this interval that could not be tied to a file name.
    pub unresolved_file_bytes: u64,
    pub file_bytes_total: u64,
}

/// Exact Windows version of the machine the collector runs on. Much of what ProcLens reads is
/// undocumented and can change between builds, so features are tied to this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OsInfo {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    /// Update build revision, the number after the dot in "19045.7725".
    pub ubr: u32,
    pub arch: String,
    /// e.g. "22H2" (empty if the registry doesn't say).
    pub display_version: String,
    /// e.g. "Core" (Home), "Professional".
    pub edition: String,
}

/// How much we know about this exact build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// Same build and patch level as a machine a profile was verified on.
    Exact,
    /// Same build number, different patch level: probably fine; self-tests decide.
    SameBuildOtherPatch,
    /// No profile for this build. Everything not self-tested is unverified.
    Untested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    /// Self-test passed on this machine just now.
    Ok,
    /// Self-test ran and failed. It is shown on the This PC tab and as a warning on the affected view;\n    /// only the memory breakdown also turns itself off. Treat a failed check as a reason to distrust\n    /// that feature's numbers.
    Failed,
    /// Not checked at startup (e.g. needs a trace session that isn't running).
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub id: String,
    pub status: CapabilityStatus,
    /// Human-readable evidence, e.g. the numbers the self-test compared.
    pub detail: String,
    /// Whether the matched profile lists this capability as verified by hand on that build.
    pub verified_in_profile: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemProfile {
    pub os: OsInfo,
    pub verification: Verification,
    /// Title of the matched profile, if any.
    pub profile_title: Option<String>,
    pub capabilities: Vec<Capability>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_shape_is_stable() {
        // The JSON shape is the contract; this guards against accidental renames.
        let r = Request::Subscribe { topic: Topic::Memory, interval_ms: 1000 };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"type":"subscribe","topic":"memory","interval_ms":1000}"#
        );
        let r: Request = serde_json::from_str(r#"{"type":"shutdown"}"#).unwrap();
        assert_eq!(r, Request::Shutdown);
    }

    #[test]
    fn unknown_request_type_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Request>(r#"{"type":"kill_process","pid":4}"#).is_err());
    }
}
