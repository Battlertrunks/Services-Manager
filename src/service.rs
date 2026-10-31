//! A service's live runtime state: process identity, status, logs, and metrics.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

use chrono::{DateTime, Local};

use crate::text::{clean_line, truncate_chars};
use crate::workspace::{ServiceSpec, extract_port};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Ready,
    Error,
    Stopping,
}

impl ServiceStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Error => "error",
            Self::Stopping => "stopping",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub at: DateTime<Local>,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct LogSnapshot {
    pub lines: Vec<LogLine>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LogMeta {
    pub len: usize,
    pub dropped: u64,
    pub first_seq: u64,
}

#[derive(Debug)]
pub struct LogBuffer {
    lines: VecDeque<LogLine>,
    capacity: usize,
    dropped: u64,
    next_seq: u64,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(512)),
            capacity: capacity.max(1),
            dropped: 0,
            next_seq: 0,
        }
    }

    pub fn push(&mut self, line: LogLine) {
        while self.lines.len() >= self.capacity {
            self.lines.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.lines.push_back(line);
        self.next_seq = self.next_seq.saturating_add(1);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.dropped = 0;
        self.next_seq = 0;
    }

    pub fn meta(&self) -> LogMeta {
        LogMeta {
            len: self.lines.len(),
            dropped: self.dropped,
            first_seq: self.next_seq.saturating_sub(self.lines.len() as u64),
        }
    }

    /// Clones only the visible slice of the buffer. `start` is an index into the current buffer.
    pub fn snapshot_range(&self, start: usize, count: usize) -> LogSnapshot {
        LogSnapshot {
            lines: self.lines.iter().skip(start).take(count).cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ServiceMetrics {
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub sampled: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct StopRequest {
    pub run_id: u64,
    pub pid: u32,
}

#[derive(Debug)]
pub enum RunOutcome {
    /// We asked the process to stop.
    Stopped,
    /// The process exited on its own with a successful status.
    Exited,
    /// The process failed to start, crashed, or printed a fatal error.
    Failed(String),
}

#[derive(Debug)]
struct RunInfo {
    id: u64,
    pid: u32,
    stop_requested: Arc<AtomicBool>,
}

#[derive(Debug)]
struct RuntimeInner {
    run: Option<RunInfo>,
    last_error: Option<String>,
    port: Option<u16>,
    started_at: Option<Instant>,
}

#[derive(Debug)]
pub struct ServiceRuntime {
    status: RwLock<ServiceStatus>,
    inner: Mutex<RuntimeInner>,
    logs: Mutex<LogBuffer>,
    metrics: RwLock<ServiceMetrics>,
    next_run_id: AtomicU64,
}

impl ServiceRuntime {
    fn new(log_capacity: usize, initial_port: Option<u16>) -> Self {
        Self {
            status: RwLock::new(ServiceStatus::Stopped),
            inner: Mutex::new(RuntimeInner {
                run: None,
                last_error: None,
                port: initial_port,
                started_at: None,
            }),
            logs: Mutex::new(LogBuffer::new(log_capacity)),
            metrics: RwLock::new(ServiceMetrics::default()),
            next_run_id: AtomicU64::new(1),
        }
    }

    pub fn status(&self) -> ServiceStatus {
        *read(&self.status)
    }

    pub fn is_running(&self) -> bool {
        lock(&self.inner).run.is_some()
    }

    pub fn port(&self) -> Option<u16> {
        lock(&self.inner).port
    }

    pub fn started_at(&self) -> Option<Instant> {
        lock(&self.inner).started_at
    }

    pub fn last_error(&self) -> Option<String> {
        lock(&self.inner).last_error.clone()
    }

    pub fn metrics(&self) -> ServiceMetrics {
        *read(&self.metrics)
    }

    pub fn logs_meta(&self) -> LogMeta {
        lock(&self.logs).meta()
    }

    pub fn logs_snapshot_range(&self, start: usize, count: usize) -> LogSnapshot {
        lock(&self.logs).snapshot_range(start, count)
    }

    pub fn clear_logs(&self) {
        lock(&self.logs).clear();
    }

    pub fn active_run(&self) -> Option<StopRequest> {
        lock(&self.inner).run.as_ref().map(|run| StopRequest {
            run_id: run.id,
            pid: run.pid,
        })
    }

    pub(crate) fn begin_start(&self, initial_port: Option<u16>) -> Result<(), String> {
        let mut inner = lock(&self.inner);
        if inner.run.is_some() {
            return Err("service is already running".to_string());
        }
        inner.last_error = None;
        inner.port = initial_port;
        inner.started_at = Some(Instant::now());
        *write(&self.metrics) = ServiceMetrics::default();
        *write(&self.status) = ServiceStatus::Starting;
        Ok(())
    }

    pub(crate) fn install_run(&self, pid: u32, stop_requested: Arc<AtomicBool>) -> Option<u64> {
        let mut inner = lock(&self.inner);
        if inner.run.is_some() {
            return None;
        }
        let run_id = self.next_run_id.fetch_add(1, Ordering::Relaxed);
        inner.run = Some(RunInfo {
            id: run_id,
            pid,
            stop_requested,
        });
        Some(run_id)
    }

    pub(crate) fn spawn_failed(&self, message: String) {
        let mut inner = lock(&self.inner);
        if inner.run.is_some() {
            return;
        }
        inner.started_at = None;
        inner.last_error = Some(message);
        *write(&self.metrics) = ServiceMetrics::default();
        *write(&self.status) = ServiceStatus::Error;
    }

    pub(crate) fn request_stop(&self) -> Option<StopRequest> {
        let inner = lock(&self.inner);
        let request = inner.run.as_ref().map(|run| {
            run.stop_requested.store(true, Ordering::SeqCst);
            StopRequest {
                run_id: run.id,
                pid: run.pid,
            }
        });
        if request.is_some() {
            *write(&self.status) = ServiceStatus::Stopping;
        }
        request
    }

    pub(crate) fn begin_cleanup(&self, run_id: u64) {
        let inner = lock(&self.inner);
        if !inner.run.as_ref().is_some_and(|run| run.id == run_id) {
            return;
        }
        let mut status = write(&self.status);
        if matches!(*status, ServiceStatus::Starting | ServiceStatus::Ready) {
            *status = ServiceStatus::Stopping;
        }
    }

    pub(crate) fn is_run_active(&self, run_id: u64) -> bool {
        lock(&self.inner)
            .run
            .as_ref()
            .is_some_and(|run| run.id == run_id)
    }

    pub(crate) fn finish_run(&self, run_id: u64, outcome: RunOutcome) {
        let mut inner = lock(&self.inner);
        if !inner.run.as_ref().is_some_and(|run| run.id == run_id) {
            return;
        }
        inner.run = None;
        inner.started_at = None;

        let status = match outcome {
            RunOutcome::Stopped => ServiceStatus::Stopped,
            // A clean exit after a reported error still deserves a red card.
            RunOutcome::Exited if inner.last_error.is_some() => ServiceStatus::Error,
            RunOutcome::Exited => ServiceStatus::Stopped,
            RunOutcome::Failed(message) => {
                inner.last_error.get_or_insert(message);
                ServiceStatus::Error
            }
        };
        *write(&self.metrics) = ServiceMetrics::default();
        *write(&self.status) = status;
    }

    pub(crate) fn promote_ready(&self, run_id: u64) {
        let inner = lock(&self.inner);
        if !inner.run.as_ref().is_some_and(|run| run.id == run_id) {
            return;
        }
        let mut status = write(&self.status);
        if *status == ServiceStatus::Starting {
            *status = ServiceStatus::Ready;
        }
    }

    pub(crate) fn set_metrics_for(
        &self,
        run_id: u64,
        pid: u32,
        cpu_percent: f32,
        memory_bytes: u64,
    ) {
        let inner = lock(&self.inner);
        if !inner
            .run
            .as_ref()
            .is_some_and(|run| run.id == run_id && run.pid == pid)
        {
            return;
        }
        *write(&self.metrics) = ServiceMetrics {
            cpu_percent,
            memory_bytes,
            sampled: true,
        };
    }
}

/// A discovered project plus its mutable runtime state.
#[derive(Debug)]
pub struct Service {
    pub spec: ServiceSpec,
    pub runtime: ServiceRuntime,
}

impl Service {
    pub fn new(spec: ServiceSpec, log_capacity: usize) -> Self {
        let runtime = ServiceRuntime::new(log_capacity, spec.port);
        Self { spec, runtime }
    }

    pub fn status(&self) -> ServiceStatus {
        self.runtime.status()
    }

    pub fn is_running(&self) -> bool {
        self.runtime.is_running()
    }

    pub fn port(&self) -> Option<u16> {
        self.runtime.port()
    }

    pub fn last_error(&self) -> Option<String> {
        self.runtime.last_error()
    }

    pub fn metrics(&self) -> ServiceMetrics {
        self.runtime.metrics()
    }

    pub fn logs_meta(&self) -> LogMeta {
        self.runtime.logs_meta()
    }

    pub fn logs_snapshot_range(&self, start: usize, count: usize) -> LogSnapshot {
        self.runtime.logs_snapshot_range(start, count)
    }

    pub(crate) fn append_marker(&self, text: impl Into<String>) {
        let mut logs = lock(&self.runtime.logs);
        logs.push(LogLine {
            at: Local::now(),
            text: text.into(),
        });
    }

    /// Cleans a raw output line, stores it, and derives status/port changes from it.
    pub(crate) fn ingest_log(&self, run_id: u64, raw: &str) {
        if !self.runtime.is_run_active(run_id) {
            return;
        }
        let cleaned = clean_line(raw);
        if cleaned.trim().is_empty() {
            return;
        }
        let cleaned = truncate_chars(&cleaned, 8_000);
        let lower = cleaned.to_lowercase();

        {
            let mut logs = lock(&self.runtime.logs);
            logs.push(LogLine {
                at: Local::now(),
                text: cleaned.clone(),
            });
        }

        let found_port = extract_port(&cleaned);
        let is_error = self.spec.line_is_error(&lower);
        let is_ready = self.spec.line_is_ready(&lower);

        let mut inner = lock(&self.runtime.inner);
        if !inner.run.as_ref().is_some_and(|run| run.id == run_id) {
            return;
        }
        if let Some(port) = found_port {
            inner.port = Some(port);
        }

        let mut status = write(&self.runtime.status);
        if is_error {
            if *status != ServiceStatus::Stopping {
                if inner.last_error.is_none() {
                    inner.last_error = Some(truncate_chars(&cleaned, 240));
                }
                *status = ServiceStatus::Error;
            }
        } else if is_ready && matches!(*status, ServiceStatus::Starting | ServiceStatus::Error) {
            *status = ServiceStatus::Ready;
            inner.last_error = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{CommandSpec, ProjectKind};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_service() -> Arc<Service> {
        let spec = ServiceSpec {
            id: "demo".into(),
            name: "demo".into(),
            path_label: "demo".into(),
            path: PathBuf::from("."),
            kind: ProjectKind::App,
            command: CommandSpec {
                program: "node".into(),
                args: vec!["server.js".into()],
            },
            env: BTreeMap::new(),
            ready_patterns: vec!["ready in".into(), "listening on".into()],
            error_patterns: vec!["error:".into(), "npm err!".into()],
            port: None,
            stop_timeout: Duration::from_secs(2),
        };
        Arc::new(Service::new(spec, 3))
    }

    #[test]
    fn log_buffer_is_bounded() {
        let mut buffer = LogBuffer::new(3);
        for i in 0..5 {
            buffer.push(LogLine {
                at: Local::now(),
                text: i.to_string(),
            });
        }
        let meta = buffer.meta();
        assert_eq!(meta.len, 3);
        assert_eq!(meta.dropped, 2);
        assert_eq!(meta.first_seq, 2);

        let snapshot = buffer.snapshot_range(0, 3);
        assert_eq!(snapshot.lines.len(), 3);
        assert_eq!(snapshot.lines[2].text, "4");

        let visible = buffer.snapshot_range(1, 2);
        assert_eq!(visible.lines.len(), 2);
        assert_eq!(visible.lines[0].text, "3");
        assert_eq!(visible.lines[1].text, "4");
    }

    #[test]
    fn ready_and_error_lines_drive_status() {
        let service = test_service();
        service.runtime.begin_start(None).unwrap();
        let run_id = service
            .runtime
            .install_run(123, Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(service.status(), ServiceStatus::Starting);

        service.ingest_log(run_id, "\x1b[32mready in 420ms\x1b[0m");
        assert_eq!(service.status(), ServiceStatus::Ready);

        service.ingest_log(run_id, "error: cannot find module 'left-pad'");
        assert_eq!(service.status(), ServiceStatus::Error);
        assert!(service.last_error().unwrap().contains("left-pad"));

        service.ingest_log(run_id, "ready in 10ms");
        assert_eq!(service.status(), ServiceStatus::Ready);

        service.runtime.finish_run(run_id, RunOutcome::Stopped);
        assert_eq!(service.status(), ServiceStatus::Stopped);
        assert!(!service.is_running());
    }

    #[test]
    fn detects_ports_and_keeps_logs_after_stop() {
        let service = test_service();
        service.runtime.begin_start(None).unwrap();
        let run_id = service
            .runtime
            .install_run(999, Arc::new(AtomicBool::new(false)))
            .unwrap();
        service.ingest_log(run_id, "  Local: http://localhost:5173/");
        assert_eq!(service.port(), Some(5173));
        service.runtime.finish_run(run_id, RunOutcome::Exited);
        assert_eq!(service.status(), ServiceStatus::Stopped);
        assert_eq!(service.logs_meta().len, 1);
    }

    #[test]
    fn clean_exit_after_an_error_stays_red() {
        let service = test_service();
        service.runtime.begin_start(None).unwrap();
        let run_id = service
            .runtime
            .install_run(77, Arc::new(AtomicBool::new(false)))
            .unwrap();
        service.ingest_log(run_id, "error: database unavailable");
        service.runtime.finish_run(run_id, RunOutcome::Exited);
        assert_eq!(service.status(), ServiceStatus::Error);
        assert!(service.last_error().is_some());
    }

    #[test]
    fn stale_run_messages_are_ignored() {
        let service = test_service();
        service.runtime.begin_start(None).unwrap();
        let run_id = service
            .runtime
            .install_run(12, Arc::new(AtomicBool::new(false)))
            .unwrap();
        service.runtime.finish_run(run_id, RunOutcome::Stopped);
        service.ingest_log(run_id, "ready in 1ms");
        assert_eq!(service.status(), ServiceStatus::Stopped);
        assert_eq!(service.logs_meta().len, 0);
    }
}
