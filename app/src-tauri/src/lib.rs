//! ProcLens UI backend. Runs unelevated; all privileged work happens in the separate collector
//! process, which this app launches on request (one UAC prompt) and talks to over a named pipe.
//!
//! One thread owns the pipe connection. It forwards collector data to the web UI as events and
//! carries UI commands (subscribe, shutdown) to the collector.

mod update;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use proclens_proto::client::{self, Connection, Poll};
use proclens_proto::{Request, Response, Topic};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

const ERROR_FILE_NOT_FOUND: i32 = 2;

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Status {
    Disconnected { reason: Option<String> },
    Connecting,
    Connected { pid: u32, elevated: bool, collector_version: String },
}

struct Link {
    status: Mutex<Status>,
    /// Sends UI requests to the I/O thread; `None` when not connected.
    tx: Mutex<Option<mpsc::Sender<Request>>>,
    /// True while an I/O thread exists, so a double click can't start two UAC prompts.
    busy: AtomicBool,
    /// Only a collector this app launched is shut down when the window closes.
    launched_by_us: AtomicBool,
}

fn set_status(app: &AppHandle, status: Status) {
    *app.state::<Link>().status.lock().unwrap() = status.clone();
    let _ = app.emit("collector-status", status);
}

/// Where the collector binary should be. Dev builds share a target directory, so it sits next to
/// this executable; an installed build ships it alongside too. `PROCLENS_COLLECTOR` overrides.
///
/// Security note: the client only trusts a pipe served by exactly this file. That is meaningful
/// when the file lives somewhere a normal user can't write (Program Files); a dev build in a
/// user-writable folder gets the check but not that guarantee.
fn collector_path() -> PathBuf {
    if let Ok(p) = std::env::var("PROCLENS_COLLECTOR") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent().map(|d| d.join("collector.exe")).unwrap_or_else(|| PathBuf::from("collector.exe"))
}

fn friendly(e: &std::io::Error, exe: &std::path::Path) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied if e.to_string().contains("declined") => {
            "Administrator approval was declined.".to_string()
        }
        std::io::ErrorKind::NotFound => format!("Collector not found at {}", exe.display()),
        std::io::ErrorKind::TimedOut => "The collector did not start in time.".to_string(),
        _ => e.to_string(),
    }
}

/// Connects (launching the collector first if `launch`), then serves the connection until it ends.
fn run_link(app: AppHandle, launch: bool) {
    let exe = collector_path();
    if launch {
        set_status(&app, Status::Connecting);
    }
    let result = match client::connect(&exe, "proclens-ui") {
        Err(e) if launch && e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
            app.state::<Link>().launched_by_us.store(true, Ordering::SeqCst);
            // 30 s idle-exit: if this UI dies, the elevated process doesn't linger.
            client::launch_elevated(&exe, "serve --idle-exit 30")
                .and_then(|()| client::connect_with_retry(&exe, "proclens-ui", Duration::from_secs(30)))
        }
        other => other,
    };
    match result {
        Ok(conn) => serve_connection(&app, conn),
        // Silent auto-connect at startup: "no collector running" is the normal case, not an error.
        Err(e) if !launch && e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
            set_status(&app, Status::Disconnected { reason: None })
        }
        Err(e) => {
            app.state::<Link>().launched_by_us.store(false, Ordering::SeqCst);
            set_status(&app, Status::Disconnected { reason: Some(friendly(&e, &exe)) })
        }
    }
}

fn serve_connection(app: &AppHandle, mut conn: Connection) {
    let link = app.state::<Link>();
    let (tx, rx) = mpsc::channel::<Request>();
    *link.tx.lock().unwrap() = Some(tx);

    if let Response::HelloAck { collector_version, collector_pid, elevated, .. } = &conn.ack {
        set_status(
            app,
            Status::Connected {
                pid: *collector_pid,
                elevated: *elevated,
                collector_version: collector_version.clone(),
            },
        );
    }
    let _ = conn.send(&Request::Subscribe { topic: Topic::Memory, interval_ms: 1000 });
    let _ = conn.send(&Request::Subscribe { topic: Topic::Processes, interval_ms: 2000 });
    let _ = conn.send(&Request::Snapshot { topic: Topic::Profile });

    let mut asked_to_stop = false;
    'io: loop {
        while let Ok(req) = rx.try_recv() {
            asked_to_stop |= req == Request::Shutdown;
            if conn.send(&req).is_err() {
                break 'io;
            }
        }
        match conn.poll(Duration::from_millis(50)) {
            Ok(Poll::Message(Response::Memory(m))) => {
                let _ = app.emit("memory", m);
            }
            Ok(Poll::Message(Response::Processes(p))) => {
                let _ = app.emit("processes", p);
            }
            Ok(Poll::Message(Response::Profile(p))) => {
                let _ = app.emit("profile", p);
            }
            Ok(Poll::Message(Response::Activity(a))) => {
                let _ = app.emit("activity", a);
            }
            Ok(Poll::Message(Response::Error { message })) => {
                let _ = app.emit("collector-error", message);
            }
            Ok(Poll::Message(Response::Bye)) | Ok(Poll::Closed) | Err(_) => break 'io,
            Ok(Poll::Message(_)) | Ok(Poll::Nothing) => {}
        }
    }

    *link.tx.lock().unwrap() = None;
    link.launched_by_us.store(false, Ordering::SeqCst);
    let reason = (!asked_to_stop).then(|| "The collector stopped.".to_string());
    set_status(app, Status::Disconnected { reason });
}

/// Starts the connect/serve thread unless one is already running.
fn spawn_link(app: &AppHandle, launch: bool) -> Result<(), String> {
    let link = app.state::<Link>();
    if link.busy.swap(true, Ordering::SeqCst) {
        return Err("already connecting or connected".into());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        run_link(app.clone(), launch);
        app.state::<Link>().busy.store(false, Ordering::SeqCst);
    });
    Ok(())
}

#[tauri::command]
fn get_status(link: State<Link>) -> Status {
    link.status.lock().unwrap().clone()
}

/// Launches the elevated collector (UAC prompt) and connects to it.
#[tauri::command]
fn start_collector(app: AppHandle) -> Result<(), String> {
    spawn_link(&app, true)
}

/// Turns the collector's kernel trace on or off. The trace is costly, so the UI only asks for it
/// while a view that needs it (Disk, Drivers) is on screen.
#[tauri::command]
fn set_activity(link: State<Link>, enabled: bool) {
    if let Some(tx) = &*link.tx.lock().unwrap() {
        let req = if enabled {
            Request::Subscribe { topic: Topic::Activity, interval_ms: proclens_proto::ACTIVITY_INTERVAL_MS }
        } else {
            Request::Unsubscribe { topic: Topic::Activity }
        };
        let _ = tx.send(req);
    }
}

#[tauri::command]
fn stop_collector(link: State<Link>) {
    if let Some(tx) = &*link.tx.lock().unwrap() {
        let _ = tx.send(Request::Shutdown);
    }
}

/// Name of the mutex that tells the installer the app is open. Must match `AppMutex` in
/// `installer/proclens.iss`.
#[cfg(windows)]
const RUNNING_MUTEX: &str = "ProcLens-App-Running";

/// Holds a named mutex for as long as the process lives, so setup and the uninstaller can refuse to
/// replace or delete `proclens-app.exe` while it is open. Failure is not fatal: the app still runs,
/// the installer just can't detect it.
#[cfg(windows)]
fn mark_running() {
    use windows::core::HSTRING;
    use windows::Win32::System::Threading::CreateMutexW;
    // The handle is deliberately never closed: Windows releases it when the process exits.
    let _ = unsafe { CreateMutexW(None, false, &HSTRING::from(RUNNING_MUTEX)) };
}

pub fn run() {
    #[cfg(windows)]
    mark_running();
    tauri::Builder::default()
        .manage(Link {
            status: Mutex::new(Status::Disconnected { reason: None }),
            tx: Mutex::new(None),
            busy: AtomicBool::new(false),
            launched_by_us: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            start_collector,
            stop_collector,
            set_activity,
            update::check_for_update,
            update::open_project_page,
            update::open_release_page
        ])
        .setup(|app| {
            // If a collector is already running (started by hand), attach silently.
            let _ = spawn_link(app.handle(), false);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { .. } = event {
                let app = window.app_handle();
                let link = app.state::<Link>();
                if link.launched_by_us.load(Ordering::SeqCst) {
                    if let Some(tx) = &*link.tx.lock().unwrap() {
                        let _ = tx.send(Request::Shutdown);
                    }
                    // Give the I/O thread (polls every 50 ms) time to deliver it. If it doesn't,
                    // the collector's 30 s idle-exit is the backstop.
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running ProcLens");
}
