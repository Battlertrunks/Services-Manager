//! Child-process supervision.
//!
//! Every service is launched in its own process group so that stopping a dev server also stops
//! the node process it spawned.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};

use crate::service::{RunOutcome, Service, ServiceStatus};
use crate::workspace::CommandSpec;

/// Process groups that have been spawned and not yet cleaned up. The flag in each entry becomes
/// true once the group is gone. This lets a panic hook clean up child dev servers and lets stop
/// escalation happen even after the direct npm process has exited.
static ACTIVE_GROUPS: LazyLock<Mutex<HashMap<u32, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn register_process_group(pid: u32) -> Arc<AtomicBool> {
    let cleaned = Arc::new(AtomicBool::new(false));
    ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(pid, Arc::clone(&cleaned));
    cleaned
}

fn group_cleanup_flag(pid: u32) -> Option<Arc<AtomicBool>> {
    ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&pid)
        .cloned()
}

fn mark_group_cleaned(pid: u32, cleaned: &AtomicBool) {
    cleaned.store(true, Ordering::SeqCst);
    unregister_process_group(pid);
}

fn unregister_process_group(pid: u32) {
    ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&pid);
}

/// Best-effort cleanup for panics and shutdown. Returns how many process groups were signalled.
pub fn kill_all_process_groups_now() -> usize {
    let groups: Vec<u32> = ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .copied()
        .collect();
    for pid in &groups {
        terminate_process_group(*pid, true);
    }
    groups.len()
}

/// Starts a service. The caller is responsible for checking whether it is already running.
pub fn start_service(service: &Arc<Service>, ready_timeout: Duration) -> Result<(), String> {
    if service.runtime.active_run().is_some() {
        return Err(format!("{} is already running", service.spec.name));
    }

    let command = &service.spec.command;
    service
        .runtime
        .begin_start(service.spec.port)
        .map_err(|_| format!("{} is already running", service.spec.name))?;
    service.append_marker(format!("── run started · {} ──", command.display()));

    let mut child = match spawn_child(service, command) {
        Ok(child) => child,
        Err(err) => {
            let message = format!("failed to start `{}`: {err}", command.display());
            service.append_marker(format!("✖ {message}"));
            service.runtime.spawn_failed(message.clone());
            return Err(message);
        }
    };

    let Some(pid) = child.id() else {
        let message = format!("`{}` started without a process id", command.display());
        let _ = child.start_kill();
        service.append_marker(format!("✖ {message}"));
        service.runtime.spawn_failed(message.clone());
        return Err(message);
    };

    let group_cleaned = register_process_group(pid);
    let stop_requested = Arc::new(AtomicBool::new(false));
    let Some(run_id) = service
        .runtime
        .install_run(pid, Arc::clone(&stop_requested))
    else {
        let _ = child.start_kill();
        terminate_process_group(pid, true);
        mark_group_cleaned(pid, &group_cleaned);
        let message = format!("{} is already running", service.spec.name);
        return Err(message);
    };

    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(tokio::spawn(read_stream(
            stdout,
            Arc::clone(service),
            run_id,
        )));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(tokio::spawn(read_stream(
            stderr,
            Arc::clone(service),
            run_id,
        )));
    }

    spawn_waiter(
        Arc::clone(service),
        child,
        run_id,
        pid,
        Arc::clone(&stop_requested),
        group_cleaned,
        readers,
        service.spec.stop_timeout,
    );
    spawn_ready_fallback(Arc::clone(service), run_id, ready_timeout);

    Ok(())
}

fn spawn_child(service: &Service, command: &CommandSpec) -> std::io::Result<Child> {
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args)
        .current_dir(&service.spec.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for (key, value) in &service.spec.env {
        cmd.env(key, value);
    }
    // Keep output readable in the TUI. ANSI stripping is a second line of defence.
    cmd.env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("CLICOLOR", "0")
        .env("CLICOLOR_FORCE", "0");

    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    cmd.spawn()
}

async fn read_stream<R>(reader: R, service: Arc<Service>, run_id: u64)
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => service.ingest_log(run_id, &line),
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_waiter(
    service: Arc<Service>,
    mut child: Child,
    run_id: u64,
    pid: u32,
    stop_requested: Arc<AtomicBool>,
    group_cleaned: Arc<AtomicBool>,
    readers: Vec<JoinHandle<()>>,
    stop_timeout: Duration,
) {
    tokio::spawn(async move {
        let exit_status = child.wait().await;
        let requested = stop_requested.load(Ordering::SeqCst);

        // Even when the direct npm process is gone, descendants may still be attached to the
        // process group. Clean the whole group before reporting the run as finished.
        service.runtime.begin_cleanup(run_id);
        cleanup_process_group(pid, stop_timeout, Arc::clone(&group_cleaned)).await;
        drain_readers(readers).await;

        let outcome = match exit_status {
            Ok(_status) if requested => RunOutcome::Stopped,
            Ok(status) if status.success() => RunOutcome::Exited,
            Ok(status) => RunOutcome::Failed(exit_description(status)),
            Err(err) if requested => {
                let _ = err;
                RunOutcome::Stopped
            }
            Err(err) => RunOutcome::Failed(format!("failed waiting for process: {err}")),
        };
        let marker = match &outcome {
            RunOutcome::Stopped => "── process stopped ──".to_string(),
            RunOutcome::Exited => "── process exited (code 0) ──".to_string(),
            RunOutcome::Failed(message) => format!("── {message} ──"),
        };
        service.append_marker(marker);
        service.runtime.finish_run(run_id, outcome);
    });
}

async fn cleanup_process_group(pid: u32, grace: Duration, cleaned: Arc<AtomicBool>) {
    if cleaned.load(Ordering::SeqCst) {
        return;
    }

    #[cfg(unix)]
    {
        if !process_group_exists(pid) {
            mark_group_cleaned(pid, &cleaned);
            return;
        }

        terminate_process_group(pid, false);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if cleaned.load(Ordering::SeqCst) {
                return;
            }
            if !process_group_exists(pid) {
                mark_group_cleaned(pid, &cleaned);
                return;
            }
            sleep(Duration::from_millis(40)).await;
        }

        terminate_process_group(pid, true);
        let kill_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < kill_deadline {
            if !process_group_exists(pid) {
                mark_group_cleaned(pid, &cleaned);
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
        // If the group still exists, leave it registered so panic/shutdown cleanup can retry.
    }

    #[cfg(not(unix))]
    {
        terminate_process_group(pid, true);
        mark_group_cleaned(pid, &cleaned);
    }
}

async fn drain_readers(readers: Vec<JoinHandle<()>>) {
    let deadline = Instant::now() + Duration::from_millis(300);
    for handle in readers {
        let abort = handle.abort_handle();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if tokio::time::timeout(remaining, handle).await.is_err() {
            abort.abort();
        }
    }
}

fn spawn_ready_fallback(service: Arc<Service>, run_id: u64, ready_timeout: Duration) {
    tokio::spawn(async move {
        sleep(ready_timeout).await;
        if service.runtime.is_run_active(run_id)
            && service.runtime.status() == ServiceStatus::Starting
        {
            service.runtime.promote_ready(run_id);
        }
    });
}

fn exit_description(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("process exited with code {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("process terminated by signal {signal}");
        }
    }
    "process exited".to_string()
}

/// Asks a running service to stop. Escalates to SIGKILL after the service's stop timeout.
pub fn stop_service(service: &Arc<Service>) {
    let Some(request) = service.runtime.request_stop() else {
        return;
    };
    terminate_process_group(request.pid, false);

    if let Some(cleaned) = group_cleanup_flag(request.pid) {
        let grace = service.spec.stop_timeout;
        tokio::spawn(async move {
            sleep(grace).await;
            if cleaned.load(Ordering::SeqCst) {
                return;
            }
            #[cfg(unix)]
            {
                if process_group_exists(request.pid) {
                    terminate_process_group(request.pid, true);
                }
            }
            #[cfg(not(unix))]
            {
                terminate_process_group(request.pid, true);
            }
        });
    }
}

#[cfg(unix)]
fn process_group_exists(pid: u32) -> bool {
    // SAFETY: signal 0 performs existence/permission checking only.
    let result = unsafe { libc::kill(-(pid as i32), 0) };
    if result == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    )
}

#[cfg(not(unix))]
fn process_group_exists(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn terminate_process_group(pid: u32, force: bool) {
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: kill(2) is safe to call with any pid; a dead process group simply returns ESRCH.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

#[cfg(windows)]
fn terminate_process_group(pid: u32, _force: bool) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(not(any(unix, windows)))]
fn terminate_process_group(_pid: u32, _force: bool) {}

/// Stops every running service and waits for their configured stop timeouts. Returns how many
/// services were running.
pub async fn shutdown_all(services: &[Arc<Service>]) -> usize {
    let running: Vec<Arc<Service>> = services
        .iter()
        .filter(|service| service.runtime.active_run().is_some())
        .cloned()
        .collect();

    for service in &running {
        stop_service(service);
    }

    let max_grace = running
        .iter()
        .map(|service| service.spec.stop_timeout)
        .max()
        .unwrap_or_default();
    let deadline = Instant::now() + max_grace + Duration::from_secs(2);
    while running
        .iter()
        .any(|service| service.runtime.active_run().is_some())
        && Instant::now() < deadline
    {
        sleep(Duration::from_millis(80)).await;
    }

    for service in &running {
        if let Some(request) = service.runtime.active_run() {
            terminate_process_group(request.pid, true);
        }
    }

    if !running.is_empty() {
        sleep(Duration::from_millis(250)).await;
    }
    running.len()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::service::Service;
    use crate::workspace::{CommandSpec, ProjectKind, ServiceSpec};
    use std::collections::BTreeMap;
    use std::path::Path;

    fn shell_service(script: &str, stop_timeout: Duration) -> Arc<Service> {
        let spec = ServiceSpec {
            id: "shell-test".into(),
            name: "shell-test".into(),
            path_label: "shell-test".into(),
            path: std::env::current_dir().unwrap(),
            kind: ProjectKind::App,
            command: CommandSpec {
                program: "sh".into(),
                args: vec!["-c".into(), script.into()],
            },
            env: BTreeMap::new(),
            ready_patterns: vec!["ready in".into()],
            error_patterns: vec!["error:".into()],
            port: None,
            stop_timeout,
        };
        Arc::new(Service::new(spec, 500))
    }

    async fn wait_for_status(service: &Service, status: ServiceStatus) {
        let deadline = Instant::now() + Duration::from_secs(4);
        while service.status() != status && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(service.status(), status);
    }

    async fn wait_for_file(path: &Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                return pid;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("pid file was not written: {}", path.display());
    }

    fn process_is_alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks for existence.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[tokio::test]
    async fn starts_reports_ready_and_stops() {
        let service = shell_service(
            "echo 'ready in 5ms'; echo 'Local: http://localhost:3987/'; sleep 30",
            Duration::from_secs(1),
        );
        start_service(&service, Duration::from_secs(2)).unwrap();
        assert!(service.is_running());

        wait_for_status(&service, ServiceStatus::Ready).await;
        assert_eq!(service.port(), Some(3987));
        let log_count = service.logs_meta().len;
        assert!(
            service
                .logs_snapshot_range(0, log_count)
                .lines
                .iter()
                .any(|line| line.text.contains("ready in"))
        );

        stop_service(&service);
        wait_for_status(&service, ServiceStatus::Stopped).await;
        assert!(!service.is_running());
        assert!(service.logs_meta().len > 0);
    }

    #[tokio::test]
    async fn detects_failed_start() {
        let service = shell_service("echo 'error: boom'; exit 3", Duration::from_secs(1));
        start_service(&service, Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        while service.status() != ServiceStatus::Error && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(service.status(), ServiceStatus::Error);
        assert!(service.last_error().is_some());
        assert!(!service.is_running());
    }

    #[tokio::test]
    async fn kills_children_that_outlive_the_parent() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("child.pid");
        let script = format!(
            "trap '' TERM; sleep 30 & echo $! > '{}'; echo 'ready in 5ms'; sleep 0.2; exit 0",
            pid_file.display()
        );
        let service = shell_service(&script, Duration::from_secs(1));
        start_service(&service, Duration::from_secs(2)).unwrap();
        let child_pid = wait_for_file(&pid_file).await;
        assert!(process_is_alive(child_pid));

        wait_for_status(&service, ServiceStatus::Stopped).await;
        assert!(
            !process_is_alive(child_pid),
            "child process survived cleanup"
        );
    }

    #[tokio::test]
    async fn stop_escalates_when_the_parent_ignores_sigterm() {
        let service = shell_service(
            "trap '' TERM; echo 'ready in 5ms'; sleep 30",
            Duration::from_secs(1),
        );
        start_service(&service, Duration::from_secs(2)).unwrap();
        wait_for_status(&service, ServiceStatus::Ready).await;

        stop_service(&service);
        wait_for_status(&service, ServiceStatus::Stopped).await;
        assert!(!service.is_running());
    }

    #[tokio::test]
    async fn shutdown_all_respects_configured_grace() {
        let service = shell_service("echo 'ready in 5ms'; sleep 30", Duration::from_secs(1));
        start_service(&service, Duration::from_secs(2)).unwrap();
        wait_for_status(&service, ServiceStatus::Ready).await;
        assert_eq!(shutdown_all(&[Arc::clone(&service)]).await, 1);
        assert!(!service.is_running());
    }
}
