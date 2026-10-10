# Vendored crates

## ferrisetw 1.2.0 (MIT OR Apache-2.0, https://github.com/n4r1b/ferrisetw)
Copied unmodified from crates.io, then patched:

- `src/native/etw_types.rs`: real-time sessions now set `PROCESS_TRACE_MODE_RAW_TIMESTAMP`, so
  `EventRecord::raw_timestamp()` returns QPC ticks (session ClientContext = 1) instead of FILETIME.
  Needed to compute DPC/ISR durations (`raw_timestamp - InitialTime`). `EventRecord::timestamp()` is
  invalid with this patch.

To re-apply on a new upstream version, diff against the crates.io tarball.
