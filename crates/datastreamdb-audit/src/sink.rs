//! Audit event sinks: callbacks, channels, stdout, and files.
//!
//! Sinks are per-instance objects (no globals), so embedded hosts can
//! isolate streams. [`FileSink`] appends JSON lines with size/age
//! rotation; compression and encryption of rotated files arrive with
//! the encryption slice.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::event::AuditEvent;

/// Receives audit events (all methods must be cheap and total).
pub trait AuditSink: Send + Sync {
    /// Records one event; failures are reported, never raised.
    fn emit(&self, event: &AuditEvent);

    /// Flushes buffered output; default is a no-op.
    fn flush(&self) {}
}

/// Sink forwarding events to a callback (embedded mode).
pub struct FnSink<F: Fn(&AuditEvent) + Send + Sync> {
    /// Callback invoked per event.
    callback: F,
}

impl<F: Fn(&AuditEvent) + Send + Sync> FnSink<F> {
    /// Wraps `callback` as a sink.
    pub fn new(callback: F) -> Self {
        Self { callback }
    }
}

impl<F: Fn(&AuditEvent) + Send + Sync> AuditSink for FnSink<F> {
    fn emit(&self, event: &AuditEvent) {
        (self.callback)(event);
    }
}

/// Channel side of [`ChannelSink`].
pub type ChannelSender = std::sync::mpsc::SyncSender<AuditEvent>;

/// Sink forwarding events over an `mpsc` channel (embedded streaming).
#[derive(Debug, Clone)]
pub struct ChannelSink {
    /// Channel sender (bounded callers drop on lag, like Go).
    sender: ChannelSender,
}

impl ChannelSink {
    /// Wraps `sender` as a sink.
    #[must_use]
    pub fn new(sender: ChannelSender) -> Self {
        Self { sender }
    }
}

impl AuditSink for ChannelSink {
    fn emit(&self, event: &AuditEvent) {
        // Non-blocking: lagging receivers drop, like Go's buffered pump.
        let _ = self.sender.try_send(event.clone());
    }
}

/// Sink writing JSON lines to stdout.
#[derive(Debug, Default)]
pub struct StdoutSink {
    /// Private lock serializing lines.
    lock: Mutex<()>,
}

impl StdoutSink {
    /// Creates the stdout sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl AuditSink for StdoutSink {
    fn emit(&self, event: &AuditEvent) {
        let _guard = self.lock.lock();
        if let Ok(line) = serde_json::to_string(event) {
            println!("{line}");
        }
    }
}

/// Append-only JSONL file sink with size/age rotation.
pub struct FileSink {
    /// State guarded as one unit (rotation must see exact sizes).
    state: Mutex<FileState>,
}

/// Mutable file-sink state.
struct FileState {
    /// Base path events append to.
    path: PathBuf,
    /// Open handle.
    file: File,
    /// Bytes written to the current file.
    bytes: u64,
    /// When the current file started.
    started: Instant,
    /// Rotate past this many bytes (0 disables).
    max_bytes: u64,
    /// Rotate past this age (zero disables).
    max_age: Duration,
}

/// Rotation errors.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// Underlying I/O failure.
    #[error("audit file error: {0}")]
    Io(String),
}

impl FileSink {
    /// Opens (creating) `path` for appends.
    ///
    /// # Errors
    ///
    /// Returns [`FileError`] when the file cannot be opened.
    pub fn open(path: &Path, max_bytes: u64, max_age: Duration) -> Result<Self, FileError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| FileError::Io(err.to_string()))?;
        let bytes = file.metadata().map_or(0, |meta| meta.len());
        Ok(Self {
            state: Mutex::new(FileState {
                path: path.to_owned(),
                file,
                bytes,
                started: Instant::now(),
                max_bytes,
                max_age,
            }),
        })
    }

    /// Current log path (for tests).
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.state
            .lock()
            .map(|state| state.path.clone())
            .unwrap_or_default()
    }
}

impl AuditSink for FileSink {
    fn emit(&self, event: &AuditEvent) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let oversized = state.max_bytes > 0 && state.bytes >= state.max_bytes;
        let expired = !state.max_age.is_zero() && state.started.elapsed() >= state.max_age;
        if oversized || expired {
            rotate(&mut state);
        }
        let Ok(mut line) = serde_json::to_string(event) else {
            return;
        };
        line.push('\n');
        if state.file.write_all(line.as_bytes()).is_ok() {
            state.bytes += line.len() as u64;
            let _ = state.file.flush();
        }
    }
}

/// Rotates to `path.1`, shifting older generations up (keeps 9).
fn rotate(state: &mut FileState) {
    let base = state.path.to_string_lossy().into_owned();
    for generation in (1..9u32).rev() {
        let from = format!("{base}.{generation}");
        let to = format!("{base}.{}", generation + 1);
        let _ = std::fs::rename(&from, &to);
    }
    let _ = std::fs::rename(&state.path, format!("{base}.1"));
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state.path)
    {
        state.file = file;
        state.bytes = 0;
        state.started = Instant::now();
    }
}

/// Shared sink handle.
pub type SharedSink = Arc<dyn AuditSink>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn event() -> AuditEvent {
        AuditEvent {
            user: "groot".to_owned(),
            namespace: 0,
            server_host: "s".to_owned(),
            client_host: "c".to_owned(),
            endpoint: "/e".to_owned(),
            req_type: "Grpc".to_owned(),
            req: "r".to_owned(),
            status: "OK".to_owned(),
            query_params: HashMap::new(),
        }
    }

    #[test]
    fn callback_sink_fires() {
        let count = Arc::new(AtomicUsize::new(0));
        let moved = count.clone();
        let sink = FnSink::new(move |_| {
            moved.fetch_add(1, Ordering::SeqCst);
        });
        sink.emit(&event());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn channel_sink_delivers() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(8);
        let sink = ChannelSink::new(sender);
        sink.emit(&event());
        assert_eq!(receiver.recv().expect("event").user, "groot");
    }

    #[test]
    fn file_sink_appends_jsonl() {
        let dir = std::env::temp_dir().join(format!("audit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("audit.log");
        let sink = FileSink::open(&path, 0, Duration::ZERO).expect("open");
        sink.emit(&event());
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"user\":\"groot\""));
        assert!(text.ends_with('\n'));
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn file_sink_rotates_on_size() {
        let dir = std::env::temp_dir().join(format!("audit-rotate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("audit.log");
        let sink = FileSink::open(&path, 1, Duration::ZERO).expect("open");
        sink.emit(&event());
        sink.emit(&event());
        assert!(dir.join("audit.log.1").exists());
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
