//! Optional CPU and memory sampling for each service's whole process tree.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::service::Service;

pub struct MetricsMonitor {
    shutdown: Arc<AtomicBool>,
    roster: Arc<RwLock<Vec<Arc<Service>>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MetricsMonitor {
    pub fn start(services: Vec<Arc<Service>>, interval: Duration) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let roster = Arc::new(RwLock::new(services));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_roster = Arc::clone(&roster);
        let handle = std::thread::Builder::new()
            .name("services-metrics".to_string())
            .spawn(move || monitor_loop(thread_roster, interval, thread_shutdown))
            .ok();

        Self {
            shutdown,
            roster,
            handle,
        }
    }

    /// Replaces the service roster, e.g. after a workspace rescan.
    pub fn update_services(&self, services: Vec<Arc<Service>>) {
        *self
            .roster
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = services;
    }

    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for MetricsMonitor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

fn monitor_loop(
    roster: Arc<RwLock<Vec<Arc<Service>>>>,
    interval: Duration,
    shutdown: Arc<AtomicBool>,
) {
    let Ok(mut system) = System::new() else {
        return;
    };

    while !shutdown.load(Ordering::SeqCst) {
        let services = roster
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut roots: Vec<(u64, u32, Arc<Service>)> = Vec::new();
        for service in services {
            if let Some(run) = service.runtime.active_run() {
                roots.push((run.run_id, run.pid, service));
            }
        }

        if roots.is_empty() {
            sleep_interruptible(interval, &shutdown);
            continue;
        }

        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );

        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (pid, process) in system.processes() {
            if let Some(parent) = process.parent() {
                children.entry(parent).or_default().push(*pid);
            }
        }

        for (run_id, root, service) in &roots {
            let mut stack = vec![Pid::from_u32(*root)];
            let mut seen = HashSet::new();
            let mut cpu = 0.0_f32;
            let mut memory = 0_u64;

            while let Some(pid) = stack.pop() {
                if !seen.insert(pid) {
                    continue;
                }
                if let Some(process) = system.process(pid) {
                    cpu += process.cpu_usage();
                    memory = memory.saturating_add(process.memory());
                }
                if let Some(child_pids) = children.get(&pid) {
                    stack.extend(child_pids.iter().copied());
                }
            }

            service.runtime.set_metrics_for(*run_id, *root, cpu, memory);
        }

        sleep_interruptible(interval, &shutdown);
    }
}

fn sleep_interruptible(duration: Duration, shutdown: &AtomicBool) {
    let step = Duration::from_millis(250);
    let mut remaining = duration;
    while !remaining.is_zero() {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let sleep_for = remaining.min(step);
        std::thread::sleep(sleep_for);
        remaining = remaining.saturating_sub(sleep_for);
    }
}
