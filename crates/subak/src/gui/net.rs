//! The window's connection to the Host: one reader thread and one writer thread per connection,
//! so the window's thread never waits on the pipe.
//!
//! The reader connects first — like the CLI, with the identity checks and the handshake, as an
//! IPC client of kind `gui` — starting a Host on demand only when asked (the first connection; a
//! Host it starts runs no shell). Then it reads frames until the connection ends and queues them
//! for the window, waking it with one posted message per batch. Idle, both threads block in the
//! kernel: no timer, no polling (`SUBAK-NFR-001`). Every message carries the connection's
//! generation, so a late message of an old connection is ignored.

use crate::client::{self, Identity, Plan, Session};
use crate::proto::{self, FLAG_INPUT, FLAG_SNAPSHOT, Frame};
use crate::win::pipe::Pipe;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the reader tells the window.
#[derive(Debug)]
pub enum Msg {
    /// Connected and verified; frames follow. `started` is the pid of a Host this connection
    /// started.
    Connected {
        link: Link,
        host_pid: u32,
        started: Option<u32>,
    },
    /// The connection attempt failed (stable CLI error code, message, hint).
    Failed {
        code: &'static str,
        message: String,
        hint: Option<String>,
    },
    Control(Value),
    Data {
        stream: u32,
        snapshot: bool,
        bytes: Vec<u8>,
    },
    /// The connection ended.
    Closed,
}

/// Messages for the window, and the wake-up it gets for them.
pub struct Inbox {
    queue: Mutex<VecDeque<(u64, Msg)>>,
    posted: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for Inbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inbox")
            .field("posted", &self.posted)
            .finish_non_exhaustive()
    }
}

impl Inbox {
    pub fn new(wake: Box<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            posted: AtomicBool::new(false),
            wake,
        })
    }

    fn push(&self, generation: u64, m: Msg) {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back((generation, m));
        // One wake-up per batch: the window drains everything that is queued.
        if !self.posted.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }

    /// Takes everything queued (the window calls this when woken).
    pub fn drain(&self) -> Vec<(u64, Msg)> {
        self.posted.store(false, Ordering::Release);
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }
}

/// The writing end of a connection: frames go to the writer thread in order.
#[derive(Debug, Clone)]
pub struct Link {
    tx: mpsc::Sender<Vec<u8>>,
    pipe: Arc<Pipe>,
}

impl Link {
    pub fn send(&self, msg: &Value) -> bool {
        self.tx.send(proto::encode_control(msg)).is_ok()
    }

    /// Input for the pane of `stream` (split into frames of at most 32 KiB, as the CLI does).
    pub fn input(&self, stream: u32, bytes: &[u8]) -> bool {
        bytes.chunks(32 * 1024).all(|c| {
            self.tx
                .send(proto::encode_data(stream, 0, FLAG_INPUT, c))
                .is_ok()
        })
    }

    /// Ends the connection: pending reads and writes fail, both threads end.
    pub fn close(&self) {
        self.pipe.cancel();
    }
}

/// Where the Host program is: `subak.exe` next to this executable.
pub fn host_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    Some(me.with_file_name(client::HOST_IMAGE))
}

/// Starts a connection attempt (generation `generation`) on its own thread. With `start`, a Host
/// is started on demand when none serves `endpoint` (one without a shell); without it, a missing
/// Host is just a failure (a reconnection never starts one).
pub fn connect(endpoint: String, start: bool, generation: u64, inbox: Arc<Inbox>) {
    connect_as(endpoint, start, generation, inbox, Identity::GUI);
}

/// [`connect`] as another client identity (the floating icon is `subak-float`).
pub fn connect_as(
    endpoint: String,
    start: bool,
    generation: u64,
    inbox: Arc<Inbox>,
    who: Identity,
) {
    std::thread::spawn(move || {
        let opened = if start {
            match host_exe() {
                Some(exe) if exe.is_file() => {
                    let plan = Plan {
                        shell: None,
                        idle_exit_s: crate::host::DEFAULT_IDLE_EXIT_S,
                    };
                    client::connect_or_start_as(&endpoint, &plan, who, &exe)
                        .map(|c| (c.session, c.started))
                }
                exe => Err(crate::output::CliError::new(
                    crate::output::Code::SpawnFailed,
                    format!(
                        "cannot start a Host: {} was not found next to subak-gui",
                        exe.map_or_else(|| "subak.exe".into(), |e| e.display().to_string())
                    ),
                )
                .hint("install subak.exe and subak-gui.exe in the same folder")),
            }
        } else {
            Session::try_open_as(&endpoint, Duration::from_millis(500), who)
                .map(|s| (s, None))
                .map_err(|f| f.err)
        };
        let (session, started) = match opened {
            Ok(s) => s,
            Err(e) => {
                inbox.push(
                    generation,
                    Msg::Failed {
                        code: e.code.as_str(),
                        message: e.message,
                        hint: e.hint,
                    },
                );
                return;
            }
        };
        let pipe = Arc::clone(&session.pipe);
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let writer = Arc::clone(&pipe);
        std::thread::spawn(move || {
            // Ends when the window drops its link or the pipe breaks.
            while let Ok(frame) = rx.recv() {
                if writer.write_all_bytes(&frame).is_err() {
                    writer.cancel();
                    break;
                }
            }
        });
        inbox.push(
            generation,
            Msg::Connected {
                link: Link {
                    tx,
                    pipe: Arc::clone(&pipe),
                },
                host_pid: session.host_pid,
                started,
            },
        );
        loop {
            match proto::read_frame(&mut &*pipe, None, |t| t + proto::FRAME_TIMEOUT) {
                Ok(Some(Frame::Control(m))) => inbox.push(generation, Msg::Control(m)),
                Ok(Some(Frame::Data {
                    stream,
                    flags,
                    bytes,
                    ..
                })) => inbox.push(
                    generation,
                    Msg::Data {
                        stream,
                        snapshot: flags & FLAG_SNAPSHOT != 0,
                        bytes,
                    },
                ),
                Ok(None) | Err(_) => break,
            }
        }
        inbox.push(generation, Msg::Closed);
    });
}
