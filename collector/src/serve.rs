//! `collector serve`: the long-running elevated process the UI talks to.
//!
//! One thread accepts connections; each client gets one thread. A client thread both reads
//! requests and writes pushes, so it must not block in a read (a synchronous pipe handle
//! serialises I/O: a blocked ReadFile would stall our WriteFile). It polls `PeekNamedPipe` instead
//! and feeds only the bytes already waiting to a `FrameReader`, so a peer that stops halfway
//! through a frame is dropped after `FRAME_STALL` instead of hanging its thread.
//!
//! The kernel trace behind [`Topic::Activity`] is expensive (it sees ~10^5 events/s and uses
//! nonpaged buffers), so it only runs while at least one client is subscribed.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use proclens_proto::frame::{is_desync, write_frame, FrameReader};
use proclens_proto::{ActivitySnapshot, Request, Response, Topic, ACTIVITY_INTERVAL_MS, PIPE_NAME, PROTOCOL_VERSION};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Pipes::PeekNamedPipe;

use crate::activity;
use crate::mem;
use crate::pipe::{is_elevated, PipeServer};
use crate::procs::Sampler;
use crate::trace::{driver_table, TraceSession};

const POLL: Duration = Duration::from_millis(20);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// A client that has started a frame must finish it within this long.
const FRAME_STALL: Duration = Duration::from_secs(5);
/// More simultaneous connections than this are closed at once (one thread each otherwise).
const MAX_CLIENTS: usize = 16;
const MIN_INTERVAL_MS: u32 = 250;
const MAX_INTERVAL_MS: u32 = 60_000;

#[derive(Default)]
struct HubInner {
    session: Option<Arc<TraceSession>>,
    subscribers: usize,
    latest: Option<Arc<ActivitySnapshot>>,
    /// Incremented for every new snapshot; clients compare it to know what they've already sent.
    seq: u64,
    ticker_stop: Option<Arc<AtomicBool>>,
    ticker: Option<JoinHandle<()>>,
}

/// Owns the (single, shared) trace session and its snapshot ticker.
#[derive(Default)]
struct Hub {
    /// Serialises start and stop so a new session never overlaps one that's still shutting down.
    gate: Mutex<()>,
    inner: Mutex<HubInner>,
}

struct Shared {
    sampler: Sampler,
    /// Computed once at startup: the OS doesn't change under us, and the self-tests are slow.
    profile: proclens_proto::SystemProfile,
    hub: Hub,
    shutdown: AtomicBool,
    clients: AtomicUsize,
    /// When the client count last dropped to zero (or process start).
    idle_since: Mutex<Instant>,
}

/// Decrements the client count when a connection ends, however it ends.
struct ClientGuard(Arc<Shared>);
impl ClientGuard {
    fn new(shared: &Arc<Shared>) -> Self {
        shared.clients.fetch_add(1, Ordering::SeqCst);
        ClientGuard(shared.clone())
    }
}
impl Drop for ClientGuard {
    fn drop(&mut self) {
        if self.0.clients.fetch_sub(1, Ordering::SeqCst) == 1 {
            *self.0.idle_since.lock().unwrap() = Instant::now();
        }
    }
}

/// A client's claim on the trace; the trace stops when the last claim goes away.
struct ActivityLease {
    shared: Arc<Shared>,
    held: bool,
}
impl ActivityLease {
    fn acquire(&mut self) -> Result<(), String> {
        if !self.held {
            acquire_trace(&self.shared)?;
            self.held = true;
        }
        Ok(())
    }
    fn release(&mut self) {
        if self.held {
            self.held = false;
            release_trace(&self.shared);
        }
    }
}
impl Drop for ActivityLease {
    fn drop(&mut self) {
        self.release();
    }
}

fn acquire_trace(shared: &Arc<Shared>) -> Result<(), String> {
    let _gate = shared.hub.gate.lock().unwrap();
    let first = shared.hub.inner.lock().unwrap().subscribers == 0;
    if first {
        if !is_elevated() {
            return Err("the kernel trace needs an elevated collector".into());
        }
        let selection = crate::profile::etw_selection();
        eprintln!("trace: event tables from {}", selection.source);
        let session = Arc::new(TraceSession::start("ProcLensTrace", selection.params.clone())?);
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = {
            let (shared, session, stop) = (shared.clone(), session.clone(), stop.clone());
            std::thread::spawn(move || tick_loop(shared, session, stop))
        };
        let mut h = shared.hub.inner.lock().unwrap();
        h.session = Some(session);
        h.ticker_stop = Some(stop);
        h.ticker = Some(ticker);
        h.latest = None;
        eprintln!("trace started");
    }
    shared.hub.inner.lock().unwrap().subscribers += 1;
    Ok(())
}

fn release_trace(shared: &Arc<Shared>) {
    let _gate = shared.hub.gate.lock().unwrap();
    let (stop, ticker, session) = {
        let mut h = shared.hub.inner.lock().unwrap();
        if h.subscribers == 0 {
            return;
        }
        h.subscribers -= 1;
        if h.subscribers > 0 {
            return;
        }
        h.latest = None;
        (h.ticker_stop.take(), h.ticker.take(), h.session.take())
    };
    stop_trace_parts(stop, ticker, session);
}

/// Stops the ticker and the session. Called with the hub lock *not* held (the ticker takes it).
fn stop_trace_parts(stop: Option<Arc<AtomicBool>>, ticker: Option<JoinHandle<()>>, session: Option<Arc<TraceSession>>) {
    if let Some(s) = stop {
        s.store(true, Ordering::SeqCst);
    }
    if let Some(t) = ticker {
        let _ = t.join(); // the ticker drops its clone of the session as it exits
    }
    if let Some(s) = session {
        if let Ok(s) = Arc::try_unwrap(s) {
            s.stop();
            eprintln!("trace stopped");
        }
    }
}

/// Called on the way out of `run` (process::exit skips destructors, which would leave the kernel
/// session running).
fn shutdown_trace(shared: &Arc<Shared>) {
    let _gate = shared.hub.gate.lock().unwrap();
    let (stop, ticker, session) = {
        let mut h = shared.hub.inner.lock().unwrap();
        h.subscribers = 0;
        (h.ticker_stop.take(), h.ticker.take(), h.session.take())
    };
    stop_trace_parts(stop, ticker, session);
}

fn tick_loop(shared: Arc<Shared>, session: Arc<TraceSession>, stop: Arc<AtomicBool>) {
    let every = Duration::from_millis(ACTIVITY_INTERVAL_MS as u64);
    let mut next = Instant::now() + every;
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
        if Instant::now() < next {
            continue;
        }
        next += every;
        let iv = session.take_interval();
        let health = session.health();
        let names: HashMap<u32, String> =
            shared.sampler.processes().processes.into_iter().map(|p| (p.pid, p.name)).collect();
        // Drivers can load at any time, so rebuild these each interval (both are cheap).
        let snap = activity::build(
            &iv,
            &names,
            health.as_ref(),
            session.qpc_freq,
            &driver_table(),
            &activity::volume_map(),
        );
        let mut h = shared.hub.inner.lock().unwrap();
        h.latest = Some(Arc::new(snap));
        h.seq += 1;
    }
}

/// Returns a process exit code. `idle_exit`: quit after this long with no clients, so an elevated
/// process the UI launched can't linger forever if the UI crashes.
pub fn run(idle_exit: Option<Duration>, also_allow: Vec<String>) -> i32 {
    eprintln!("collector serve: elevated={}", is_elevated());
    let mut server = match PipeServer::new(PIPE_NAME, &also_allow) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not build pipe security: {e}");
            return 1;
        }
    };
    let profile = crate::profile::build();
    eprintln!(
        "collector serve: Windows {}.{}.{} ({:?}); self-tests: {}",
        profile.os.major,
        profile.os.build,
        profile.os.ubr,
        profile.verification,
        profile
            .capabilities
            .iter()
            .map(|c| format!("{}={:?}", c.id, c.status))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let shared = Arc::new(Shared {
        sampler: Sampler::new(),
        profile,
        hub: Hub::default(),
        shutdown: AtomicBool::new(false),
        clients: AtomicUsize::new(0),
        idle_since: Mutex::new(Instant::now()),
    });

    // The accept thread reports whether the *first* instance could be created (squat detection).
    let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut first = true;
            loop {
                match server.accept() {
                    Ok(file) => {
                        if first {
                            first = false;
                            let _ = ready_tx.send(Ok(()));
                        }
                        if shared.clients.load(Ordering::SeqCst) >= MAX_CLIENTS {
                            eprintln!("too many clients ({MAX_CLIENTS}); closing a new connection");
                            continue; // `file` drops here, which closes this pipe instance
                        }
                        // Counted before the thread starts, so the idle-exit check in `run` can
                        // never see zero clients while a connection is being set up.
                        let guard = ClientGuard::new(&shared);
                        let shared = shared.clone();
                        std::thread::spawn(move || {
                            if let Err(e) = handle_client(file, &shared) {
                                eprintln!("client ended: {e}");
                            }
                            drop(guard);
                        });
                    }
                    Err(e) => {
                        if first {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                        eprintln!("accept failed: {e}");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        });
    }
    // accept() blocks until the first client, so a *successful* start looks like silence here.
    // A failed first CreateNamedPipe returns immediately; wait briefly to catch that case.
    match ready_rx.recv_timeout(Duration::from_millis(500)) {
        Ok(Err(e)) => {
            eprintln!(
                "could not create the collector pipe (is another collector running, or is the name taken?): {e}"
            );
            return 1;
        }
        _ => eprintln!("collector serve: listening on {PIPE_NAME}"),
    }

    loop {
        std::thread::sleep(Duration::from_millis(250));
        if shared.shutdown.load(Ordering::SeqCst) {
            eprintln!("collector serve: shutdown requested");
            shutdown_trace(&shared);
            return 0;
        }
        if let Some(limit) = idle_exit {
            if shared.clients.load(Ordering::SeqCst) == 0
                && shared.idle_since.lock().unwrap().elapsed() >= limit
            {
                eprintln!("collector serve: idle for {limit:?}, exiting");
                shutdown_trace(&shared);
                return 0;
            }
        }
    }
}

/// Number of bytes waiting to be read; an error means the client is gone.
fn available(file: &File) -> io::Result<u32> {
    let mut avail = 0u32;
    unsafe {
        PeekNamedPipe(HANDLE(file.as_raw_handle()), None, 0, None, Some(&mut avail), None)
    }
    .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
    Ok(avail)
}

fn data_for(topic: Topic, shared: &Shared) -> Response {
    match topic {
        Topic::Memory => match mem::collect() {
            Ok(m) => Response::Memory(m),
            Err(message) => Response::Error { message },
        },
        Topic::Processes => Response::Processes(shared.sampler.processes()),
        Topic::Profile => Response::Profile(shared.profile.clone()),
        Topic::Activity => match shared.hub.inner.lock().unwrap().latest.clone() {
            Some(s) => Response::Activity((*s).clone()),
            None => Response::Error { message: "no trace is running; subscribe to activity first".into() },
        },
    }
}

/// Reads at most one request, never blocking. `Ok(None)`: no complete request yet. An `Err` of
/// kind `InvalidData` that is not `is_desync` is a malformed message in an intact stream; any
/// other error (client gone, stalled mid-frame, oversized frame) means the connection must end.
fn next_request(file: &mut File, reader: &mut FrameReader) -> io::Result<Option<Request>> {
    let avail = available(file)? as usize;
    let req = reader.poll::<_, Request>(file, avail)?;
    if req.is_none() && reader.stalled_for().is_some_and(|d| d > FRAME_STALL) {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "client stopped in the middle of a message"));
    }
    Ok(req)
}

/// A client's self-chosen name, shortened and quoted so it can't forge or flood log lines.
fn log_name(client: &str) -> String {
    let short: String = client.chars().take(64).collect();
    format!("{short:?}")
}

fn handle_client(mut file: File, shared: &Arc<Shared>) -> io::Result<()> {
    let mut reader = FrameReader::new();

    // 1. Handshake: the first frame must be Hello with a matching protocol version.
    let started = Instant::now();
    let first = loop {
        match next_request(&mut file, &mut reader) {
            Ok(Some(req)) => break req,
            Ok(None) => {
                if started.elapsed() > HELLO_TIMEOUT {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no Hello"));
                }
                std::thread::sleep(POLL);
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData && !is_desync(&e) => {
                write_frame(&mut file, &Response::Error { message: "first message must be Hello".into() })?;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    };
    match first {
        Request::Hello { protocol_version, client } if protocol_version == PROTOCOL_VERSION => {
            eprintln!("client connected: {}", log_name(&client));
            write_frame(
                &mut file,
                &Response::HelloAck {
                    protocol_version: PROTOCOL_VERSION,
                    collector_version: env!("CARGO_PKG_VERSION").to_string(),
                    collector_pid: std::process::id(),
                    elevated: is_elevated(),
                },
            )?;
        }
        Request::Hello { protocol_version, .. } => {
            write_frame(
                &mut file,
                &Response::Error {
                    message: format!("unsupported protocol {protocol_version}; collector speaks {PROTOCOL_VERSION}"),
                },
            )?;
            return Ok(());
        }
        _ => {
            write_frame(&mut file, &Response::Error { message: "first message must be Hello".into() })?;
            return Ok(());
        }
    }

    // 2. Serve requests and pushes from this single thread.
    let mut subs: HashMap<Topic, (Duration, Instant)> = HashMap::new();
    let mut lease = ActivityLease { shared: shared.clone(), held: false };
    let mut last_activity_seq = 0u64;
    loop {
        if shared.shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        loop {
            let req = match next_request(&mut file, &mut reader) {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) if e.kind() == io::ErrorKind::InvalidData && !is_desync(&e) => {
                    // Malformed request inside an intact frame: tell the client, keep the connection.
                    write_frame(&mut file, &Response::Error { message: format!("bad request: {e}") })?;
                    continue;
                }
                // Client gone, stalled mid-frame, or the stream lost its frame boundary.
                Err(e) => return Err(e),
            };
            match req {
                Request::Hello { .. } => {
                    write_frame(&mut file, &Response::Error { message: "already connected".into() })?
                }
                Request::Subscribe { topic: Topic::Activity, .. } => match lease.acquire() {
                    Ok(()) => {
                        last_activity_seq = 0;
                        write_frame(
                            &mut file,
                            &Response::Subscribed { topic: Topic::Activity, interval_ms: ACTIVITY_INTERVAL_MS },
                        )?;
                    }
                    Err(message) => write_frame(&mut file, &Response::Error { message })?,
                },
                Request::Unsubscribe { topic: Topic::Activity } => {
                    lease.release();
                    write_frame(&mut file, &Response::Unsubscribed { topic: Topic::Activity })?;
                }
                Request::Subscribe { topic, interval_ms } => {
                    let ms = interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS);
                    subs.insert(topic, (Duration::from_millis(ms as u64), Instant::now()));
                    write_frame(&mut file, &Response::Subscribed { topic, interval_ms: ms })?;
                }
                Request::Unsubscribe { topic } => {
                    subs.remove(&topic);
                    write_frame(&mut file, &Response::Unsubscribed { topic })?;
                }
                Request::Snapshot { topic } => write_frame(&mut file, &data_for(topic, shared))?,
                Request::Shutdown => {
                    write_frame(&mut file, &Response::Bye)?;
                    shared.shutdown.store(true, Ordering::SeqCst);
                    return Ok(());
                }
            }
        }
        let now = Instant::now();
        for (topic, (every, next)) in subs.iter_mut() {
            if now >= *next {
                write_frame(&mut file, &data_for(*topic, shared))?;
                *next = now + *every;
            }
        }
        if lease.held {
            let (seq, snap) = {
                let h = shared.hub.inner.lock().unwrap();
                (h.seq, h.latest.clone())
            };
            if seq != last_activity_seq {
                last_activity_seq = seq;
                if let Some(s) = snap {
                    write_frame(&mut file, &Response::Activity((*s).clone()))?;
                }
            }
        }
        std::thread::sleep(POLL);
    }
}
