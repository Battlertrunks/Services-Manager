//! Application state and input handling.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::process;
use crate::service::{LogMeta, Service, ServiceStatus};
use crate::workspace::Workspace;

const MESSAGE_LIFETIME: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Logs(usize),
    Help,
    ConfirmQuit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub text: String,
    pub kind: MessageKind,
    pub at: Instant,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LogViewState {
    pub follow: bool,
    /// Sequence number of the top visible line while paused. Sequence numbers stay stable when
    /// the ring buffer drops old lines.
    pub top_seq: u64,
    pub h_scroll: u16,
    pub viewport: u16,
}

/// Maps a paused sequence anchor to an index in the current ring buffer.
pub(crate) fn log_top_index(meta: &LogMeta, viewport: u16, follow: bool, top_seq: u64) -> usize {
    let max_top = meta.len.saturating_sub((viewport as usize).max(1));
    if follow {
        max_top
    } else {
        let anchored = top_seq.saturating_sub(meta.first_seq) as usize;
        anchored.min(max_top)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CardHitArea {
    pub index: usize,
    pub rect: Rect,
    pub toggle: Option<Rect>,
    pub logs: Rect,
}

pub struct App {
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub warnings: Vec<String>,
    pub options: crate::workspace::WorkspaceOptions,
    pub services: Vec<Arc<Service>>,
    pub selected: usize,
    pub overlay: Overlay,
    pub logs: LogViewState,
    pub help_scroll: u16,
    pub grid_columns: usize,
    pub grid_visible_rows: usize,
    pub grid_offset_rows: usize,
    pub hit_areas: Vec<CardHitArea>,
    pub should_quit: bool,
    pub metrics_enabled: bool,
    message: Option<Message>,
    config_override: Option<PathBuf>,
    metrics_cli_enabled: bool,
    services_dirty: bool,
}

impl App {
    pub fn new(
        workspace: Workspace,
        config_override: Option<PathBuf>,
        metrics_cli_enabled: bool,
    ) -> Self {
        let log_capacity = workspace.options.log_capacity;
        let services = workspace
            .specs
            .into_iter()
            .map(|spec| Arc::new(Service::new(spec, log_capacity)))
            .collect::<Vec<_>>();

        let mut app = Self {
            root: workspace.root,
            config_path: workspace.config_path,
            warnings: workspace.warnings,
            metrics_enabled: workspace.options.metrics_enabled,
            options: workspace.options,
            services,
            selected: 0,
            overlay: Overlay::None,
            logs: LogViewState {
                follow: true,
                ..LogViewState::default()
            },
            help_scroll: 0,
            grid_columns: 1,
            grid_visible_rows: 1,
            grid_offset_rows: 0,
            hit_areas: Vec::new(),
            should_quit: false,
            message: None,
            config_override,
            metrics_cli_enabled,
            services_dirty: false,
        };

        let count = app.services.len();
        if count == 0 {
            app.notify(MessageKind::Warning, "No runnable projects found");
        } else if app.warnings.is_empty() {
            app.notify(
                MessageKind::Info,
                format!("Discovered {count} project{}", plural(count)),
            );
        } else {
            app.notify(
                MessageKind::Info,
                format!(
                    "Discovered {count} project{} · {} warning{} (press ?)",
                    plural(count),
                    app.warnings.len(),
                    plural(app.warnings.len())
                ),
            );
        }
        app
    }

    pub fn running_count(&self) -> usize {
        self.services
            .iter()
            .filter(|service| service.is_running())
            .count()
    }

    pub fn selected_service(&self) -> Option<&Arc<Service>> {
        self.services.get(self.selected)
    }

    pub fn take_services_dirty(&mut self) -> bool {
        std::mem::take(&mut self.services_dirty)
    }

    pub fn notify(&mut self, kind: MessageKind, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            kind,
            at: Instant::now(),
        });
    }

    pub fn current_message(&self) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|message| message.at.elapsed() < MESSAGE_LIFETIME)
    }

    pub fn tick(&mut self) {
        if self
            .message
            .as_ref()
            .is_some_and(|message| message.at.elapsed() >= MESSAGE_LIFETIME)
        {
            self.message = None;
        }
    }

    /// Called by the UI before drawing the grid. Keeps the selected card on screen after resizes.
    pub fn prepare_grid(&mut self, columns: usize, visible_rows: usize) {
        self.hit_areas.clear();
        self.grid_columns = columns.max(1);
        self.grid_visible_rows = visible_rows.max(1);
        self.ensure_selection_visible();
    }

    fn total_grid_rows(&self) -> usize {
        self.services
            .len()
            .div_ceil(self.grid_columns.max(1))
            .max(1)
    }

    pub fn ensure_selection_visible(&mut self) {
        if self.services.is_empty() {
            self.selected = 0;
            self.grid_offset_rows = 0;
            return;
        }
        self.selected = self.selected.min(self.services.len() - 1);
        let row = self.selected / self.grid_columns.max(1);
        let visible = self.grid_visible_rows.max(1);
        let max_offset = self.total_grid_rows().saturating_sub(visible);

        if row < self.grid_offset_rows {
            self.grid_offset_rows = row;
        } else if row >= self.grid_offset_rows + visible {
            self.grid_offset_rows = row + 1 - visible;
        }
        self.grid_offset_rows = self.grid_offset_rows.min(max_offset);
    }

    pub fn navigate(&mut self, dx: isize, dy: isize) {
        if self.services.is_empty() {
            return;
        }
        let columns = self.grid_columns.max(1);
        let col = self.selected % columns;
        let mut next = self.selected;

        if dx < 0 && col > 0 {
            next -= 1;
        } else if dx > 0 && col + 1 < columns && next + 1 < self.services.len() {
            next += 1;
        }

        if dy < 0 && next >= columns {
            next -= columns;
        } else if dy > 0 && next + columns < self.services.len() {
            next += columns;
        }

        self.selected = next.min(self.services.len() - 1);
        self.ensure_selection_visible();
    }

    pub fn select_relative(&mut self, delta: isize) {
        if self.services.is_empty() {
            return;
        }
        let len = self.services.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
        self.ensure_selection_visible();
    }

    pub fn select_first(&mut self) {
        if !self.services.is_empty() {
            self.selected = 0;
            self.ensure_selection_visible();
        }
    }

    pub fn select_last(&mut self) {
        if !self.services.is_empty() {
            self.selected = self.services.len() - 1;
            self.ensure_selection_visible();
        }
    }

    pub fn toggle_selected(&mut self) {
        let Some(service) = self.selected_service().cloned() else {
            return;
        };
        if service.is_running() {
            process::stop_service(&service);
            self.notify(MessageKind::Info, format!("Stopping {}", service.spec.name));
        } else {
            match process::start_service(&service, self.options.ready_timeout) {
                Ok(()) => self.notify(MessageKind::Info, format!("Starting {}", service.spec.name)),
                Err(err) => self.notify(MessageKind::Error, err),
            }
        }
    }

    pub fn start_all(&mut self) {
        let mut started = 0;
        let mut failed = 0;
        let mut first_error = None;
        for service in &self.services {
            if service.is_running() {
                continue;
            }
            match process::start_service(service, self.options.ready_timeout) {
                Ok(()) => started += 1,
                Err(err) => {
                    failed += 1;
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
            }
        }

        if failed > 0 {
            self.notify(
                MessageKind::Warning,
                format!(
                    "Started {started}; {failed} failed{}",
                    first_error
                        .map(|err| format!(": {err}"))
                        .unwrap_or_default()
                ),
            );
        } else if started > 0 {
            self.notify(
                MessageKind::Success,
                format!("Starting {started} service{}", plural(started)),
            );
        } else {
            self.notify(MessageKind::Info, "All services are already running");
        }
    }

    pub fn stop_all(&mut self) {
        let mut stopped = 0;
        for service in &self.services {
            if service.is_running() {
                process::stop_service(service);
                stopped += 1;
            }
        }
        if stopped > 0 {
            self.notify(
                MessageKind::Info,
                format!("Stopping {stopped} service{}", plural(stopped)),
            );
        } else {
            self.notify(MessageKind::Info, "No services are running");
        }
    }

    pub fn request_quit(&mut self) {
        if self.running_count() > 0 {
            self.overlay = Overlay::ConfirmQuit;
        } else {
            self.should_quit = true;
        }
    }

    pub fn confirm_quit(&mut self) {
        self.should_quit = true;
    }

    pub fn open_logs(&mut self, index: usize) {
        if index >= self.services.len() {
            return;
        }
        self.selected = index;
        self.overlay = Overlay::Logs(index);
        self.logs = LogViewState {
            follow: true,
            top_seq: 0,
            h_scroll: 0,
            viewport: self.logs.viewport,
        };
    }

    pub fn scroll_logs(&mut self, delta: isize) {
        let Overlay::Logs(index) = self.overlay else {
            return;
        };
        let Some(service) = self.services.get(index) else {
            return;
        };
        let meta = service.logs_meta();
        let max_top = meta
            .len
            .saturating_sub((self.logs.viewport as usize).max(1));
        let current = log_top_index(
            &meta,
            self.logs.viewport,
            self.logs.follow,
            self.logs.top_seq,
        );
        let next = (current as isize + delta).clamp(0, max_top as isize) as usize;
        self.logs.top_seq = meta.first_seq.saturating_add(next as u64);
        self.logs.follow = next >= max_top;
    }

    pub fn scroll_logs_page(&mut self, down: bool) {
        let page = (self.logs.viewport.max(5) as isize) - 1;
        self.scroll_logs(if down { page } else { -page });
    }

    pub fn logs_to_top(&mut self) {
        if let Overlay::Logs(index) = self.overlay
            && let Some(service) = self.services.get(index)
        {
            self.logs.top_seq = service.logs_meta().first_seq;
            self.logs.follow = false;
        }
    }

    pub fn logs_to_bottom(&mut self) {
        if matches!(self.overlay, Overlay::Logs(_)) {
            self.logs.follow = true;
            self.logs.top_seq = 0;
        }
    }

    pub fn toggle_log_follow(&mut self) {
        if !matches!(self.overlay, Overlay::Logs(_)) {
            return;
        }
        if self.logs.follow {
            if let Overlay::Logs(index) = self.overlay
                && let Some(service) = self.services.get(index)
            {
                let meta = service.logs_meta();
                let max_top = meta
                    .len
                    .saturating_sub((self.logs.viewport as usize).max(1));
                self.logs.top_seq = meta.first_seq.saturating_add(max_top as u64);
            }
            self.logs.follow = false;
        } else {
            self.logs.follow = true;
            self.logs.top_seq = 0;
        }
    }

    pub fn pan_logs(&mut self, delta: i16) {
        if !matches!(self.overlay, Overlay::Logs(_)) {
            return;
        }
        self.logs.h_scroll = (self.logs.h_scroll as i32 + delta as i32).clamp(0, 500) as u16;
    }

    pub fn clear_logs(&mut self) {
        if let Overlay::Logs(index) = self.overlay
            && let Some(service) = self.services.get(index)
        {
            service.runtime.clear_logs();
            self.logs.follow = true;
            self.logs.top_seq = 0;
            self.notify(
                MessageKind::Info,
                format!("Cleared logs for {}", service.spec.name),
            );
        }
    }

    pub fn rescan(&mut self) {
        if self.running_count() > 0 {
            self.notify(
                MessageKind::Warning,
                "Stop all running services before rescanning",
            );
            return;
        }

        let workspace = match Workspace::load(
            &self.root,
            self.config_override.as_deref(),
            self.metrics_cli_enabled,
        ) {
            Ok(workspace) => workspace,
            Err(err) => {
                self.notify(MessageKind::Error, format!("Rescan failed: {err}"));
                return;
            }
        };

        let selected_id = self
            .selected_service()
            .map(|service| service.spec.id.clone());
        let metrics_changed = workspace.options.metrics_enabled != self.metrics_enabled
            || (self.metrics_enabled
                && workspace.options.metrics_interval != self.options.metrics_interval);
        let log_capacity_changed = workspace.options.log_capacity != self.options.log_capacity;
        let active_metrics_enabled = self.metrics_enabled;
        let active_metrics_interval = self.options.metrics_interval;

        let mut previous: HashMap<String, Arc<Service>> = std::mem::take(&mut self.services)
            .into_iter()
            .map(|service| (service.spec.id.clone(), service))
            .collect();
        let new_count = workspace.specs.len();
        let mut new_services = Vec::with_capacity(new_count);
        let mut preserved = 0;
        let mut updated = 0;
        for spec in workspace.specs {
            let id = spec.id.clone();
            let service = if let Some(existing) = previous.remove(&id) {
                if existing.spec == spec && !log_capacity_changed {
                    preserved += 1;
                    existing
                } else {
                    updated += 1;
                    Arc::new(Service::new(spec, workspace.options.log_capacity))
                }
            } else {
                Arc::new(Service::new(spec, workspace.options.log_capacity))
            };
            new_services.push(service);
        }

        self.services = new_services;
        self.warnings = workspace.warnings;
        self.config_path = workspace.config_path;
        self.options = workspace.options;
        if metrics_changed {
            // The sampler is created at startup. Keep reporting the settings that are actually
            // active, and tell the user how to apply the new ones.
            self.options.metrics_enabled = active_metrics_enabled;
            self.options.metrics_interval = active_metrics_interval;
            self.warnings.push(
                "Metrics settings changed in config; restart services-manager to apply them."
                    .to_string(),
            );
        }
        self.selected = selected_id
            .and_then(|id| {
                self.services
                    .iter()
                    .position(|service| service.spec.id == id)
            })
            .unwrap_or_else(|| self.selected.min(self.services.len().saturating_sub(1)));
        self.grid_offset_rows = 0;
        self.overlay = Overlay::None;
        self.services_dirty = true;

        let added = self.services.len().saturating_sub(preserved + updated);
        let removed = previous.len();
        let mut message = format!(
            "Rescanned: {} service{} ({added} new, {updated} updated, {removed} removed)",
            self.services.len(),
            plural(self.services.len())
        );
        if metrics_changed {
            message.push_str(" · restart for metrics changes");
        }
        self.notify(
            if metrics_changed {
                MessageKind::Warning
            } else {
                MessageKind::Info
            },
            message,
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            if self.overlay == Overlay::ConfirmQuit {
                self.confirm_quit();
            } else {
                self.request_quit();
            }
            return;
        }

        match self.overlay {
            Overlay::ConfirmQuit => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirm_quit(),
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = Overlay::None
                    }
                    _ => {}
                }
                return;
            }
            Overlay::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                        self.overlay = Overlay::None
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.help_scroll = self.help_scroll.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.help_scroll = self.help_scroll.saturating_add(1)
                    }
                    KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                    KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10),
                    KeyCode::Home => self.help_scroll = 0,
                    _ => {}
                }
                return;
            }
            Overlay::Logs(_) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
                    KeyCode::Up | KeyCode::Char('k') => self.scroll_logs(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.scroll_logs(1),
                    KeyCode::PageUp => self.scroll_logs_page(false),
                    KeyCode::PageDown => self.scroll_logs_page(true),
                    KeyCode::Home | KeyCode::Char('g') => self.logs_to_top(),
                    KeyCode::End | KeyCode::Char('G') => self.logs_to_bottom(),
                    KeyCode::Char('f') => self.toggle_log_follow(),
                    KeyCode::Char('c') => self.clear_logs(),
                    KeyCode::Left | KeyCode::Char('h') => self.pan_logs(-4),
                    KeyCode::Right | KeyCode::Char('l') => self.pan_logs(4),
                    _ => {}
                }
                return;
            }
            Overlay::None => {}
        }

        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('?') => {
                self.overlay = Overlay::Help;
                self.help_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.navigate(0, -1),
            KeyCode::Down | KeyCode::Char('j') => self.navigate(0, 1),
            KeyCode::Left | KeyCode::Char('h') => self.navigate(-1, 0),
            KeyCode::Right | KeyCode::Char('l') => self.navigate(1, 0),
            KeyCode::Tab => self.select_relative(1),
            KeyCode::BackTab => self.select_relative(-1),
            KeyCode::Home => self.select_first(),
            KeyCode::End => self.select_last(),
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('s') => self.toggle_selected(),
            KeyCode::Char('o') => self.open_logs(self.selected),
            KeyCode::Char('a') => self.start_all(),
            KeyCode::Char('x') => self.stop_all(),
            KeyCode::Char('r') => self.rescan(),
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => match self.overlay {
                Overlay::None => self.navigate(0, -1),
                Overlay::Logs(_) => self.scroll_logs(-3),
                Overlay::Help => self.help_scroll = self.help_scroll.saturating_sub(3),
                Overlay::ConfirmQuit => {}
            },
            MouseEventKind::ScrollDown => match self.overlay {
                Overlay::None => self.navigate(0, 1),
                Overlay::Logs(_) => self.scroll_logs(3),
                Overlay::Help => self.help_scroll = self.help_scroll.saturating_add(3),
                Overlay::ConfirmQuit => {}
            },
            MouseEventKind::Down(MouseButton::Left) if self.overlay == Overlay::None => {
                let position = (mouse.column, mouse.row);
                let hit = self.hit_areas.iter().copied().find(|hit| {
                    contains(hit.rect, position)
                        || contains(hit.logs, position)
                        || hit.toggle.is_some_and(|rect| contains(rect, position))
                });
                let Some(hit) = hit else {
                    return;
                };
                self.selected = hit.index;
                self.ensure_selection_visible();
                if let Some(toggle) = hit.toggle
                    && contains(toggle, position)
                {
                    self.toggle_selected();
                } else if contains(hit.logs, position) {
                    self.open_logs(hit.index);
                }
            }
            _ => {}
        }
    }

    pub fn status_counts(&self) -> (usize, usize, usize, usize) {
        let mut ready = 0;
        let mut starting = 0;
        let mut error = 0;
        let mut stopped = 0;
        for service in &self.services {
            match service.status() {
                ServiceStatus::Ready => ready += 1,
                ServiceStatus::Starting | ServiceStatus::Stopping => starting += 1,
                ServiceStatus::Error => error += 1,
                ServiceStatus::Stopped => stopped += 1,
            }
        }
        (ready, starting, error, stopped)
    }

    pub fn help_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "services-manager".to_string(),
            "A local dashboard for MFEs and micro-services.".to_string(),
            String::new(),
            "Navigation".to_string(),
            "  ↑ ↓ ← → / h j k l    Move selection".to_string(),
            "  Tab / Shift-Tab      Cycle selection".to_string(),
            "  Enter / Space / s    Start or stop selected service".to_string(),
            "  o                    Open live logs".to_string(),
            "  a                    Start all services".to_string(),
            "  x                    Stop all services".to_string(),
            "  r                    Rescan workspace (when stopped)".to_string(),
            "  ?                    Toggle this help".to_string(),
            "  q / Ctrl-C           Quit (stops running services)".to_string(),
            String::new(),
            "Logs".to_string(),
            "  ↑ ↓ / PgUp PgDn      Scroll logs".to_string(),
            "  g / G                Jump to top / tail".to_string(),
            "  f                    Toggle follow mode".to_string(),
            "  ← →                  Pan horizontally".to_string(),
            "  c                    Clear the log buffer".to_string(),
            "  Esc / q              Close logs".to_string(),
            String::new(),
            "Mouse".to_string(),
            "  Click Start/Logs buttons on a card; wheel scrolls.".to_string(),
            String::new(),
            "Workspace".to_string(),
            format!("  root:   {}", self.root.display()),
            format!(
                "  config: {}",
                self.config_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "auto-discovery".to_string())
            ),
            "  start commands: auto-detected (dev → start → serve), then npm start / npm run <script>"
                .to_string(),
            format!(
                "  ready timeout: {}s · stop timeout: {}s · logs: {} lines",
                self.options.ready_timeout.as_secs(),
                self.options.stop_timeout.as_secs(),
                self.options.log_capacity
            ),
        ];

        if !self.warnings.is_empty() {
            lines.push(String::new());
            lines.push("Warnings".to_string());
            for warning in &self.warnings {
                lines.push(format!("  ! {warning}"));
            }
        }
        lines
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn contains(rect: Rect, position: (u16, u16)) -> bool {
    let (x, y) = position;
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{CommandSpec, ProjectKind, ServiceSpec, WorkspaceOptions};
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn service_spec(name: &str) -> ServiceSpec {
        ServiceSpec {
            id: name.to_string(),
            name: name.to_string(),
            path_label: name.to_string(),
            path: PathBuf::from("."),
            kind: ProjectKind::App,
            command: CommandSpec {
                program: "true".to_string(),
                args: Vec::new(),
            },
            env: BTreeMap::new(),
            ready_patterns: Vec::new(),
            error_patterns: Vec::new(),
            port: None,
            stop_timeout: Duration::from_secs(1),
        }
    }

    fn test_app(count: usize) -> App {
        let services = (0..count)
            .map(|index| Arc::new(Service::new(service_spec(&format!("service-{index}")), 100)))
            .collect();
        App {
            root: PathBuf::from("/tmp/workspace"),
            config_path: None,
            warnings: Vec::new(),
            options: WorkspaceOptions::default(),
            services,
            selected: 0,
            overlay: Overlay::None,
            logs: LogViewState::default(),
            help_scroll: 0,
            grid_columns: 3,
            grid_visible_rows: 2,
            grid_offset_rows: 0,
            hit_areas: Vec::new(),
            should_quit: false,
            metrics_enabled: true,
            message: None,
            config_override: None,
            metrics_cli_enabled: true,
            services_dirty: false,
        }
    }

    #[test]
    fn grid_navigation_respects_columns_and_edges() {
        let mut app = test_app(7);
        assert_eq!(app.selected, 0);
        app.navigate(1, 0);
        assert_eq!(app.selected, 1);
        app.navigate(1, 0);
        assert_eq!(app.selected, 2);
        app.navigate(1, 0);
        assert_eq!(app.selected, 2, "does not wrap within a row");
        app.navigate(0, 1);
        assert_eq!(app.selected, 5);
        app.navigate(0, 1);
        assert_eq!(app.selected, 5, "last row has no third column");
        app.navigate(0, -1);
        assert_eq!(app.selected, 2);
        app.navigate(-1, 0);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn selection_scrolls_the_grid() {
        let mut app = test_app(10);
        app.prepare_grid(3, 2);
        app.selected = 7;
        app.ensure_selection_visible();
        assert_eq!(app.grid_offset_rows, 1);

        app.selected = 0;
        app.ensure_selection_visible();
        assert_eq!(app.grid_offset_rows, 0);
    }

    #[test]
    fn tab_cycles_and_home_end_jump() {
        let mut app = test_app(4);
        app.select_relative(-1);
        assert_eq!(app.selected, 3);
        app.select_relative(1);
        assert_eq!(app.selected, 0);
        app.select_last();
        assert_eq!(app.selected, 3);
        app.select_first();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn log_scrolling_starts_following_and_pauses_when_scrolled_up() {
        let mut app = test_app(1);
        let service = app.services[0].clone();
        service.runtime.begin_start(None).unwrap();
        let run_id = service
            .runtime
            .install_run(42, Arc::new(std::sync::atomic::AtomicBool::new(false)))
            .unwrap();
        for index in 0..20 {
            service.ingest_log(run_id, &format!("line {index}"));
        }
        app.overlay = Overlay::Logs(0);
        app.logs.follow = true;
        app.logs.viewport = 5;

        app.scroll_logs(-1);
        assert!(!app.logs.follow);
        assert_eq!(app.logs.top_seq, 14);
        app.logs_to_top();
        assert_eq!(app.logs.top_seq, 0);
        app.logs_to_bottom();
        assert!(app.logs.follow);
        app.toggle_log_follow();
        assert!(!app.logs.follow);
        assert_eq!(app.logs.top_seq, 15);
    }

    #[test]
    fn renders_scrolled_log_modal_from_a_large_buffer() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use std::sync::atomic::AtomicBool;

        let mut app = test_app(1);
        let service = app.services[0].clone();
        service.runtime.begin_start(None).unwrap();
        let run_id = service
            .runtime
            .install_run(42, Arc::new(AtomicBool::new(false)))
            .unwrap();
        for index in 0..250 {
            service.ingest_log(run_id, &format!("line {index}"));
        }
        app.overlay = Overlay::Logs(0);
        app.logs.follow = true;

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();

        app.scroll_logs(-10);
        assert!(!app.logs.follow);
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        service
            .runtime
            .finish_run(run_id, crate::service::RunOutcome::Stopped);
    }

    #[test]
    fn paused_log_anchor_survives_ring_buffer_wrap() {
        let meta = LogMeta {
            len: 5,
            dropped: 3,
            first_seq: 3,
        };
        // Sequence 5 is still line index 2 after three older lines were dropped.
        assert_eq!(log_top_index(&meta, 2, false, 5), 2);
        // A dropped anchor falls back to the oldest retained line.
        assert_eq!(log_top_index(&meta, 2, false, 1), 0);
        assert_eq!(log_top_index(&meta, 2, true, 0), 3);
    }

    #[test]
    fn rescan_adds_projects_and_applies_changed_package_scripts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let app_dir = root.join("apps");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(
            app_dir.join("package.json"),
            r#"{"name":"alpha","scripts":{"dev":"vite"},"dependencies":{"vue":"^3"}}"#,
        )
        .unwrap();

        let workspace = Workspace::load(root, None, true).unwrap();
        let mut app = App::new(workspace, None, true);
        assert_eq!(app.services.len(), 1);

        std::fs::write(
            app_dir.join("package.json"),
            r#"{"name":"alpha","scripts":{"serve":"vite --port 5000"},"dependencies":{"vue":"^3"}}"#,
        )
        .unwrap();
        let beta_dir = root.join("services").join("beta");
        std::fs::create_dir_all(&beta_dir).unwrap();
        std::fs::write(
            beta_dir.join("package.json"),
            r#"{"name":"beta","scripts":{"start":"node server.js"},"dependencies":{"express":"^4"}}"#,
        )
        .unwrap();

        app.rescan();
        assert_eq!(app.services.len(), 2);
        let alpha = app
            .services
            .iter()
            .find(|service| service.spec.name == "alpha")
            .unwrap();
        assert_eq!(alpha.spec.command.display(), "npm run serve");
        assert!(
            app.current_message()
                .unwrap()
                .text
                .contains("1 new, 1 updated")
        );
        assert!(app.take_services_dirty());
    }

    #[test]
    fn rescan_recreates_services_when_log_capacity_changes_and_warns_about_metrics() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"name":"alpha","scripts":{"dev":"vite"},"dependencies":{"vue":"^3"}}"#,
        )
        .unwrap();
        let workspace = Workspace::load(root, None, true).unwrap();
        let mut app = App::new(workspace, None, true);
        let before = Arc::clone(&app.services[0]);
        assert!(app.metrics_enabled);

        std::fs::write(
            root.join("services-manager.toml"),
            "[workspace]\nlog_buffer_lines = 5000\nmetrics = false\nmetrics_interval_ms = 5000\n",
        )
        .unwrap();

        app.rescan();
        assert_eq!(app.services.len(), 1);
        assert!(
            !Arc::ptr_eq(&before, &app.services[0]),
            "changed log capacity should rebuild the service"
        );
        assert!(
            app.metrics_enabled,
            "startup metric state should remain active"
        );
        assert!(
            app.warnings
                .iter()
                .any(|warning| warning.contains("restart"))
        );
        assert_eq!(app.current_message().unwrap().kind, MessageKind::Warning);
        assert!(app.current_message().unwrap().text.contains("restart"));
    }

    #[test]
    fn quit_needs_confirmation_only_when_services_run() {
        let mut app = test_app(1);
        app.request_quit();
        assert!(app.should_quit);
        assert_eq!(app.overlay, Overlay::None);

        let mut app = test_app(1);
        app.services[0].runtime.begin_start(None).unwrap();
        app.services[0]
            .runtime
            .install_run(7, Arc::new(std::sync::atomic::AtomicBool::new(false)))
            .unwrap();
        app.request_quit();
        assert!(!app.should_quit);
        assert_eq!(app.overlay, Overlay::ConfirmQuit);
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.should_quit);
    }
}
