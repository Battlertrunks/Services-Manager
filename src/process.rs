//! Child-process supervision.
//!
//! Every service is launched in its own process group so that stopping a dev server also stops
//! the node process it spawned.

use std::collections::HashSet;
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

/// Process groups that have been spawned and not yet reaped. This lets a panic hook clean up
/// child dev servers instead of leaving them orphaned.
static ACTIVE_GROUPS: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn register_process_group(pid: u32) {
    ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(pid);
}

fn unregister_process_group(pid: u32) {
    ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&pid);
}

/// Best-effort cleanup for panics and other abrupt exits.
pub fn kill_all_process_groups_now() {
    let groups: Vec<u32> = ACTIVE_GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .copied()
        .collect();
    for pid in groups {
        terminate_process_group(pid, true);
    }
}

/// Starts a service. The caller is responsible for checking whether it is already running.
pub fn start_service(service: &Arc<Service>, ready_timeout: Duration) -> Result<(), String> {
    if service.runtime.active_run().is_some() {
        return Err(format!("{} is already running", service.spec.name));
    }

    let command = &service.spec.command;
    service.runtime.begin_start(service.spec.port);
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

    register_process_group(pid);
    let stop_requested = Arc::new(AtomicBool::new(false));
    let Some(run_id) = service
        .runtime
        .install_run(pid, Arc::clone(&stop_requested))
    else {
        let _ = child.start_kill();
        unregister_process_group(pid);
        let message = format!("{} is already running", service.spec.name);
        service.runtime.spawn_failed(message.clone());
        return Err(message);
    };

    let stdout_reader = child
        .stdout
        .take()
        .map(|stdout| tokio::spawn(read_stream(stdout, Arc::clone(service), run_id)));
    let stderr_reader = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(read_stream(stderr, Arc::clone(service), run_id)));

    spawn_waiter(
        Arc::clone(service),
        child,
        run_id,
        pid,
        Arc::clone(&stop_requested),
        stdout_reader,
        stderr_reader,
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

fn spawn_waiter(
    service: Arc<Service>,
    mut child: Child,
    run_id: u64,
    pid: u32,
    stop_requested: Arc<AtomicBool>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
) {
    tokio::spawn(async move {
        let exit_status = child.wait().await;
        let requested = stop_requested.load(Ordering::SeqCst);

        if !requested {
            // A dev server can exit while its children keep running. Clean up anything still in
            // the process group before reporting the service as stopped.
            terminate_process_group(pid, false);
        }

        // Give the stdout/stderr readers a moment to drain final lines before closing the run.
        let drain = async {
            if let Some(reader) = stdout_reader {
                let _ = reader.await;
            }
            if let Some(reader) = stderr_reader {
                let _ = reader.await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_millis(300), drain).await;

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
        unregister_process_group(pid);
        service.runtime.finish_run(run_id, outcome);
    });
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

    let service = Arc::clone(service);
    let stop_timeout = service.spec.stop_timeout;
    tokio::spawn(async move {
        sleep(stop_timeout).await;
        if service.runtime.is_run_active(request.run_id) {
            terminate_process_group(request.pid, true);
        }
    });
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

/// Stops every running service and waits briefly for them to exit. Returns how many were running.
pub async fn shutdown_all(services: &[Arc<Service>]) -> usize {
    let running: Vec<Arc<Service>> = services
        .iter()
        .filter(|service| service.runtime.active_run().is_some())
        .cloned()
        .collect();

    for service in &running {
        stop_service(service);
    }

    let started = Instant::now();
    let deadline = started + Duration::from_secs(6);
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
        sleep(Duration::from_millis(150)).await;
    }
    running.len()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::service::Service;
    use crate::workspace::{CommandSpec, ProjectKind, ServiceSpec};
    use std::collections::BTreeMap;

    fn shell_service(script: &str) -> Arc<Service> {
        let spec = ServiceSpec {
            id: "shell-test".into(),
            name: "shell-test".into(),
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
            stop_timeout: Duration::from_secs(1),
        };
        Arc::new(Service::new(spec, 50))
    }

    async fn wait_for_status(service: &Service, status: ServiceStatus) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while service.status() != status && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(service.status(), status);
    }

    #[tokio::test]
    async fn starts_reports_ready_and_stops() {
        let service =
            shell_service("echo 'ready in 5ms'; echo 'Local: http://localhost:3987/'; sleep 30");
        start_service(&service, Duration::from_secs(2)).unwrap();
        assert!(service.is_running());

        wait_for_status(&service, ServiceStatus::Ready).await;
        assert_eq!(service.port(), Some(3987));
        assert!(
            service
                .logs_snapshot()
                .lines
                .iter()
                .any(|line| line.text.contains("ready in"))
        );

        stop_service(&service);
        wait_for_status(&service, ServiceStatus::Stopped).await;
        assert!(!service.is_running());
        assert!(!service.logs_snapshot().lines.is_empty());
    }

    #[tokio::test]
    async fn detects_failed_start() {
        let service = shell_service("echo 'error: boom'; exit 3");
        start_service(&service, Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while service.status() != ServiceStatus::Error && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(service.status(), ServiceStatus::Error);
        assert!(service.last_error().is_some());
        assert!(!service.is_running());
    }
}
