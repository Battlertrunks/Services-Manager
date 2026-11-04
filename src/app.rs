//! Application state and input handling.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::bundles::{Bundle, BundleStore};
use crate::process;
use crate::service::{LogMeta, Service, ServiceStatus};
use crate::workspace::Workspace;

const MESSAGE_LIFETIME: Duration = Duration::from_secs(5);
const MAX_BUNDLE_NAME_CHARS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Logs(usize),
    Help,
    ConfirmQuit,
    Bundles,
    BundleEditor,
    BundleDelete(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BundlePane {
    #[default]
    List,
    Members,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BundleEditorFocus {
    #[default]
    Name,
    Services,
}

#[derive(Debug, Clone)]
pub struct BundleEditorState {
    pub bundle_id: Option<i64>,
    pub name: String,
    pub selected: BTreeSet<String>,
    pub cursor: usize,
    pub focus: BundleEditorFocus,
    pub error: Option<String>,
    pub missing_services: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BundleUiState {
    pub selected: usize,
    pub member_selected: usize,
    pub pane: BundlePane,
    pub editor: Option<BundleEditorState>,
    pub notice: Option<String>,
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
    pub bundles: Vec<Bundle>,
    pub active_bundle_id: Option<i64>,
    pub bundle_ui: BundleUiState,
    message: Option<Message>,
    config_override: Option<PathBuf>,
    metrics_cli_enabled: bool,
    services_dirty: bool,
    bundle_store: Option<BundleStore>,
    bundle_error: Option<String>,
    log_return: Overlay,
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
            bundles: Vec::new(),
            active_bundle_id: None,
            bundle_ui: BundleUiState::default(),
            message: None,
            config_override,
            metrics_cli_enabled,
            services_dirty: false,
            bundle_store: None,
            bundle_error: None,
            log_return: Overlay::None,
        };

        match BundleStore::open(&app.root) {
            Ok(store) => {
                app.bundle_store = Some(store);
                if let Some(error) = app.reload_bundles() {
                    app.warnings.push(error);
                }
            }
            Err(err) => {
                let message = format!("Bundles unavailable: {err}");
                app.bundle_error = Some(message.clone());
                app.warnings.push(message);
            }
        }

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

    pub fn bundle_error(&self) -> Option<&str> {
        self.bundle_error.as_deref()
    }

    pub fn bundle_db_path(&self) -> Option<&Path> {
        self.bundle_store.as_ref().map(BundleStore::path)
    }

    fn reload_bundles(&mut self) -> Option<String> {
        let Some(store) = &self.bundle_store else {
            let error = "Bundle storage is unavailable".to_string();
            self.bundle_error = Some(error.clone());
            return Some(error);
        };
        match store.list() {
            Ok(bundles) => {
                self.bundles = bundles;
                self.bundle_error = None;
                if self
                    .active_bundle_id
                    .is_some_and(|id| !self.bundles.iter().any(|bundle| bundle.id == id))
                {
                    self.active_bundle_id = None;
                }
                self.clamp_bundle_ui();
                None
            }
            Err(err) => {
                let error = format!("Bundle database error: {err}");
                self.bundle_error = Some(error.clone());
                Some(error)
            }
        }
    }

    pub fn open_bundles(&mut self) {
        let error = self.reload_bundles();
        if let Some(active_id) = self.active_bundle_id
            && let Some(position) = self
                .bundles
                .iter()
                .position(|bundle| bundle.id == active_id)
        {
            self.bundle_ui.selected = position;
        }
        self.clamp_bundle_ui();
        self.bundle_ui.notice = error.or_else(|| self.bundle_error.clone());
        self.bundle_ui.pane = BundlePane::List;
        self.bundle_ui.member_selected = 0;
        self.overlay = Overlay::Bundles;
    }

    fn clamp_bundle_ui(&mut self) {
        if self.bundles.is_empty() {
            self.bundle_ui.selected = 0;
            self.bundle_ui.member_selected = 0;
            self.bundle_ui.pane = BundlePane::List;
            return;
        }
        self.bundle_ui.selected = self.bundle_ui.selected.min(self.bundles.len() - 1);
        let member_count = self
            .bundles
            .get(self.bundle_ui.selected)
            .map(|bundle| bundle.service_ids.len())
            .unwrap_or(0);
        self.bundle_ui.member_selected = self
            .bundle_ui
            .member_selected
            .min(member_count.saturating_sub(1));
        if member_count == 0 {
            self.bundle_ui.pane = BundlePane::List;
        }
    }

    pub fn selected_bundle(&self) -> Option<&Bundle> {
        self.bundles.get(self.bundle_ui.selected)
    }

    pub fn service_index_by_id(&self, service_id: &str) -> Option<usize> {
        self.services
            .iter()
            .position(|service| service.spec.id == service_id)
    }

    pub fn bundle_status_counts(&self, bundle: &Bundle) -> (usize, usize, usize, usize, usize) {
        let mut ready = 0;
        let mut starting = 0;
        let mut error = 0;
        let mut stopped = 0;
        let mut missing = 0;
        for service_id in &bundle.service_ids {
            let Some(index) = self.service_index_by_id(service_id) else {
                missing += 1;
                continue;
            };
            match self.services[index].status() {
                ServiceStatus::Ready => ready += 1,
                ServiceStatus::Starting | ServiceStatus::Stopping => starting += 1,
                ServiceStatus::Error => error += 1,
                ServiceStatus::Stopped => stopped += 1,
            }
        }
        (ready, starting, error, stopped, missing)
    }

    pub fn open_bundle_member_logs(&mut self) {
        let Some(bundle) = self.selected_bundle() else {
            self.bundle_ui.notice = Some("No bundle selected".to_string());
            return;
        };
        let Some(service_id) = bundle.service_ids.get(self.bundle_ui.member_selected) else {
            self.bundle_ui.notice = Some("This bundle has no services".to_string());
            return;
        };
        let Some(index) = self.service_index_by_id(service_id) else {
            self.bundle_ui.notice = Some(format!(
                "Service {service_id} is no longer available. Rescan or edit the bundle."
            ));
            return;
        };
        self.open_logs_with_return(index, Overlay::Bundles);
    }

    pub fn toggle_bundle(&mut self, bundle_id: i64) {
        if self.active_bundle_id == Some(bundle_id) {
            self.disable_active_bundle();
        } else {
            self.enable_bundle(bundle_id);
        }
    }

    pub fn enable_bundle(&mut self, bundle_id: i64) {
        let Some(bundle) = self
            .bundles
            .iter()
            .find(|bundle| bundle.id == bundle_id)
            .cloned()
        else {
            self.bundle_ui.notice = Some("Bundle no longer exists".to_string());
            return;
        };

        let available: Vec<Arc<Service>> = bundle
            .service_ids
            .iter()
            .filter_map(|service_id| {
                self.service_index_by_id(service_id)
                    .map(|index| Arc::clone(&self.services[index]))
            })
            .collect();
        if available.is_empty() {
            let message = format!(
                "Cannot enable \"{}\": none of its services are available",
                bundle.name
            );
            self.bundle_ui.notice = Some(message.clone());
            self.notify(MessageKind::Error, message);
            return;
        }

        let new_ids: HashSet<&str> = bundle.service_ids.iter().map(String::as_str).collect();
        let mut to_stop = Vec::new();
        if let Some(previous_id) = self.active_bundle_id
            && previous_id != bundle_id
            && let Some(previous) = self
                .bundles
                .iter()
                .find(|previous| previous.id == previous_id)
        {
            for service_id in &previous.service_ids {
                if new_ids.contains(service_id.as_str()) {
                    continue;
                }
                if let Some(index) = self.service_index_by_id(service_id) {
                    let service = &self.services[index];
                    if service.is_running() {
                        to_stop.push(Arc::clone(service));
                    }
                }
            }
        }
        for service in &to_stop {
            process::stop_service(service);
        }

        self.active_bundle_id = Some(bundle_id);
        let mut started = 0;
        let mut already_running = 0;
        let mut failed = 0;
        for service in &available {
            if service.is_running() {
                already_running += 1;
                continue;
            }
            match process::start_service(service, self.options.ready_timeout) {
                Ok(()) => started += 1,
                Err(_) => failed += 1,
            }
        }

        let missing = bundle.service_ids.len().saturating_sub(available.len());
        let mut notice = format!("Enabled \"{}\"", bundle.name);
        if started > 0 {
            notice.push_str(&format!(" · started {started}"));
        }
        if already_running > 0 {
            notice.push_str(&format!(" · {already_running} already running"));
        }
        if failed > 0 {
            notice.push_str(&format!(" · {failed} failed"));
        }
        if missing > 0 {
            notice.push_str(&format!(" · {missing} missing"));
        }
        let kind = if failed > 0 || missing > 0 {
            MessageKind::Warning
        } else {
            MessageKind::Success
        };
        self.bundle_ui.notice = Some(notice.clone());
        self.bundle_ui.selected = self
            .bundles
            .iter()
            .position(|candidate| candidate.id == bundle_id)
            .unwrap_or(self.bundle_ui.selected);
        self.bundle_ui.member_selected = 0;
        self.notify(kind, notice);
    }

    pub fn disable_active_bundle(&mut self) {
        let Some(bundle_id) = self.active_bundle_id.take() else {
            self.bundle_ui.notice = Some("No bundle is currently enabled".to_string());
            return;
        };
        let Some(bundle) = self
            .bundles
            .iter()
            .find(|bundle| bundle.id == bundle_id)
            .cloned()
        else {
            self.bundle_ui.notice = Some("Bundle no longer exists".to_string());
            return;
        };

        let mut stopped = 0;
        for service_id in &bundle.service_ids {
            if let Some(index) = self.service_index_by_id(service_id) {
                let service = &self.services[index];
                if service.is_running() {
                    process::stop_service(service);
                    stopped += 1;
                }
            }
        }
        let notice = if stopped > 0 {
            format!("Disabled \"{}\" · stopping {stopped}", bundle.name)
        } else {
            format!("Disabled \"{}\"", bundle.name)
        };
        self.bundle_ui.notice = Some(notice.clone());
        self.notify(MessageKind::Info, notice);
    }

    pub fn delete_bundle(&mut self, bundle_id: i64) {
        let Some(bundle) = self
            .bundles
            .iter()
            .find(|bundle| bundle.id == bundle_id)
            .cloned()
        else {
            self.overlay = Overlay::Bundles;
            self.bundle_ui.notice = Some("Bundle no longer exists".to_string());
            return;
        };
        if self.active_bundle_id == Some(bundle_id) {
            self.disable_active_bundle();
        }

        let result = match self.bundle_store.as_mut() {
            Some(store) => store.delete(bundle_id),
            None => Err(anyhow::anyhow!("Bundle storage is unavailable")),
        };
        self.overlay = Overlay::Bundles;
        match result {
            Ok(()) => {
                let error = self.reload_bundles();
                self.bundle_ui.notice =
                    Some(error.unwrap_or_else(|| format!("Deleted \"{}\"", bundle.name)));
                self.notify(
                    MessageKind::Info,
                    format!("Deleted bundle \"{}\"", bundle.name),
                );
            }
            Err(err) => {
                let message = format!("Could not delete bundle: {err}");
                self.bundle_ui.notice = Some(message.clone());
                self.notify(MessageKind::Error, message);
            }
        }
    }

    pub fn open_bundle_editor(&mut self, bundle_id: Option<i64>) {
        if self.bundle_store.is_none() {
            self.bundle_ui.notice = Some("Bundle storage is unavailable".to_string());
            return;
        }
        let editor = if let Some(bundle_id) = bundle_id {
            if self.active_bundle_id == Some(bundle_id) {
                self.bundle_ui.notice = Some("Disable the bundle before editing it".to_string());
                return;
            }
            let Some(bundle) = self
                .bundles
                .iter()
                .find(|bundle| bundle.id == bundle_id)
                .cloned()
            else {
                self.bundle_ui.notice = Some("Bundle no longer exists".to_string());
                return;
            };
            let missing_services = bundle
                .service_ids
                .iter()
                .filter(|service_id| self.service_index_by_id(service_id).is_none())
                .count();
            let cursor = self
                .services
                .iter()
                .position(|service| bundle.service_ids.contains(&service.spec.id))
                .unwrap_or(0);
            BundleEditorState {
                bundle_id: Some(bundle_id),
                name: bundle.name,
                selected: bundle.service_ids.into_iter().collect(),
                cursor,
                focus: BundleEditorFocus::Name,
                error: None,
                missing_services,
            }
        } else {
            BundleEditorState {
                bundle_id: None,
                name: String::new(),
                selected: BTreeSet::new(),
                cursor: 0,
                focus: BundleEditorFocus::Name,
                error: None,
                missing_services: 0,
            }
        };
        self.bundle_ui.editor = Some(editor);
        self.bundle_ui.notice = None;
        self.overlay = Overlay::BundleEditor;
    }

    pub fn cancel_bundle_editor(&mut self) {
        self.bundle_ui.editor = None;
        self.overlay = Overlay::Bundles;
    }

    fn set_bundle_editor_error(&mut self, message: impl Into<String>) {
        if let Some(editor) = self.bundle_ui.editor.as_mut() {
            editor.error = Some(message.into());
        }
    }

    pub fn save_bundle_editor(&mut self) {
        let Some(editor) = self.bundle_ui.editor.as_ref() else {
            return;
        };
        let bundle_id = editor.bundle_id;
        let name = editor.name.trim().to_string();
        let selected = editor.selected.clone();

        if name.is_empty() {
            self.set_bundle_editor_error("Bundle name is required");
            return;
        }
        if name.chars().count() > MAX_BUNDLE_NAME_CHARS {
            self.set_bundle_editor_error(format!(
                "Bundle name must be {MAX_BUNDLE_NAME_CHARS} characters or fewer"
            ));
            return;
        }
        let lower_name = name.to_lowercase();
        if self
            .bundles
            .iter()
            .any(|bundle| Some(bundle.id) != bundle_id && bundle.name.to_lowercase() == lower_name)
        {
            self.set_bundle_editor_error(format!("A bundle named \"{name}\" already exists"));
            return;
        }

        let service_ids: Vec<String> = self
            .services
            .iter()
            .filter(|service| selected.contains(&service.spec.id))
            .map(|service| service.spec.id.clone())
            .collect();
        if service_ids.is_empty() {
            self.set_bundle_editor_error("Select at least one available service");
            return;
        }

        let result = match self.bundle_store.as_mut() {
            Some(store) => match bundle_id {
                Some(id) => store.update(id, &name, &service_ids).map(|_| id),
                None => store.create(&name, &service_ids),
            },
            None => Err(anyhow::anyhow!("Bundle storage is unavailable")),
        };
        match result {
            Ok(saved_id) => {
                self.bundle_ui.editor = None;
                self.overlay = Overlay::Bundles;
                let error = self.reload_bundles();
                self.bundle_ui.selected = self
                    .bundles
                    .iter()
                    .position(|bundle| bundle.id == saved_id)
                    .unwrap_or(self.bundle_ui.selected);
                self.bundle_ui.member_selected = 0;
                self.clamp_bundle_ui();
                self.bundle_ui.notice =
                    Some(error.unwrap_or_else(|| format!("Saved bundle \"{name}\"")));
                self.notify(MessageKind::Success, format!("Saved bundle \"{name}\""));
            }
            Err(err) => {
                self.set_bundle_editor_error(format!("Could not save bundle: {err}"));
            }
        }
    }

    fn handle_bundles_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = Overlay::None;
                self.bundle_ui.notice = None;
            }
            KeyCode::Char('n') => self.open_bundle_editor(None),
            KeyCode::Char('e') => {
                if let Some(bundle) = self.selected_bundle().map(|bundle| bundle.id) {
                    self.open_bundle_editor(Some(bundle));
                } else {
                    self.bundle_ui.notice = Some("No bundle selected".to_string());
                }
            }
            KeyCode::Char('d') => {
                if let Some(bundle) = self.selected_bundle().map(|bundle| bundle.id) {
                    self.overlay = Overlay::BundleDelete(bundle);
                } else {
                    self.bundle_ui.notice = Some("No bundle selected".to_string());
                }
            }
            KeyCode::Char('o') => self.open_bundle_member_logs(),
            KeyCode::Enter => {
                if self.bundle_ui.pane == BundlePane::Members {
                    self.open_bundle_member_logs();
                } else if let Some(bundle) = self.selected_bundle().map(|bundle| bundle.id) {
                    self.toggle_bundle(bundle);
                } else {
                    self.bundle_ui.notice = Some("Create a bundle first".to_string());
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.bundle_ui.pane == BundlePane::Members {
                    self.move_bundle_member(-1);
                } else {
                    self.move_bundle_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.bundle_ui.pane == BundlePane::Members {
                    self.move_bundle_member(1);
                } else {
                    self.move_bundle_selection(1);
                }
            }
            KeyCode::Tab => {
                if self.bundle_ui.pane == BundlePane::Members {
                    self.bundle_ui.pane = BundlePane::List;
                } else {
                    let has_members = self
                        .selected_bundle()
                        .is_some_and(|bundle| !bundle.service_ids.is_empty());
                    if has_members {
                        self.bundle_ui.pane = BundlePane::Members;
                    } else {
                        self.bundle_ui.notice = Some("This bundle has no services".to_string());
                    }
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let has_members = self
                    .selected_bundle()
                    .is_some_and(|bundle| !bundle.service_ids.is_empty());
                if has_members {
                    self.bundle_ui.pane = BundlePane::Members;
                } else {
                    self.bundle_ui.notice = Some("This bundle has no services".to_string());
                }
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.bundle_ui.pane = BundlePane::List;
            }
            _ => {}
        }
    }

    fn move_bundle_selection(&mut self, delta: isize) {
        if self.bundles.is_empty() {
            return;
        }
        let last = self.bundles.len() - 1;
        let next = (self.bundle_ui.selected as isize + delta).clamp(0, last as isize) as usize;
        if next != self.bundle_ui.selected {
            self.bundle_ui.selected = next;
            self.bundle_ui.member_selected = 0;
            self.bundle_ui.pane = BundlePane::List;
            self.bundle_ui.notice = None;
        }
    }

    fn move_bundle_member(&mut self, delta: isize) {
        let Some(bundle) = self.selected_bundle() else {
            return;
        };
        if bundle.service_ids.is_empty() {
            return;
        }
        let last = bundle.service_ids.len() - 1;
        self.bundle_ui.member_selected =
            (self.bundle_ui.member_selected as isize + delta).clamp(0, last as isize) as usize;
    }

    fn handle_bundle_editor_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('s')) {
            self.save_bundle_editor();
            return;
        }
        match key.code {
            KeyCode::Esc => self.cancel_bundle_editor(),
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(editor) = self.bundle_ui.editor.as_mut() {
                    editor.focus = match editor.focus {
                        BundleEditorFocus::Name => {
                            if self.services.is_empty() {
                                editor.error = Some("No services are available".to_string());
                                BundleEditorFocus::Name
                            } else {
                                editor.error = None;
                                BundleEditorFocus::Services
                            }
                        }
                        BundleEditorFocus::Services => BundleEditorFocus::Name,
                    };
                }
            }
            KeyCode::Enter => {
                let focus = self
                    .bundle_ui
                    .editor
                    .as_ref()
                    .map(|editor| editor.focus)
                    .unwrap_or(BundleEditorFocus::Name);
                if focus == BundleEditorFocus::Name {
                    if let Some(editor) = self.bundle_ui.editor.as_mut()
                        && !self.services.is_empty()
                    {
                        editor.focus = BundleEditorFocus::Services;
                        editor.error = None;
                    }
                } else {
                    self.toggle_editor_service();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_editor_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_editor_cursor(1),
            KeyCode::Char(' ') => {
                if self
                    .bundle_ui
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.focus == BundleEditorFocus::Services)
                {
                    self.toggle_editor_service();
                }
            }
            KeyCode::Backspace => {
                if let Some(editor) = self.bundle_ui.editor.as_mut()
                    && editor.focus == BundleEditorFocus::Name
                {
                    editor.name.pop();
                    editor.error = None;
                }
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(editor) = self.bundle_ui.editor.as_mut()
                    && editor.focus == BundleEditorFocus::Name
                    && !ch.is_control()
                    && editor.name.chars().count() < MAX_BUNDLE_NAME_CHARS
                {
                    editor.name.push(ch);
                    editor.error = None;
                }
            }
            _ => {}
        }
    }

    fn toggle_editor_service(&mut self) {
        let Some(cursor) = self.bundle_ui.editor.as_ref().map(|editor| editor.cursor) else {
            return;
        };
        let Some(service_id) = self
            .services
            .get(cursor)
            .map(|service| service.spec.id.clone())
        else {
            return;
        };
        if let Some(editor) = self.bundle_ui.editor.as_mut() {
            if !editor.selected.remove(&service_id) {
                editor.selected.insert(service_id);
            }
            editor.error = None;
        }
    }

    fn move_editor_cursor(&mut self, delta: isize) {
        if self.services.is_empty() {
            return;
        }
        if let Some(editor) = self.bundle_ui.editor.as_mut()
            && editor.focus == BundleEditorFocus::Services
        {
            let last = self.services.len() - 1;
            editor.cursor = (editor.cursor as isize + delta).clamp(0, last as isize) as usize;
        }
    }

    fn handle_bundle_delete_key(&mut self, key: KeyEvent, bundle_id: i64) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.delete_bundle(bundle_id)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = Overlay::Bundles;
            }
            _ => {}
        }
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
        self.open_logs_with_return(index, Overlay::None);
    }

    fn open_logs_with_return(&mut self, index: usize, return_to: Overlay) {
        if index >= self.services.len() {
            return;
        }
        self.selected = index;
        self.log_return = return_to;
        self.overlay = Overlay::Logs(index);
        self.logs = LogViewState {
            follow: true,
            top_seq: 0,
            h_scroll: 0,
            viewport: self.logs.viewport,
        };
    }

    fn close_logs(&mut self) {
        self.overlay = self.log_return;
        self.log_return = Overlay::None;
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
        self.log_return = Overlay::None;
        self.clamp_bundle_ui();
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
                    KeyCode::Esc | KeyCode::Char('q') => self.close_logs(),
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
            Overlay::Bundles => {
                self.handle_bundles_key(key);
                return;
            }
            Overlay::BundleEditor => {
                self.handle_bundle_editor_key(key);
                return;
            }
            Overlay::BundleDelete(bundle_id) => {
                self.handle_bundle_delete_key(key, bundle_id);
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
            KeyCode::Char('b') => self.open_bundles(),
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
                Overlay::Bundles => {
                    if self.bundle_ui.pane == BundlePane::Members {
                        self.move_bundle_member(-1);
                    } else {
                        self.move_bundle_selection(-1);
                    }
                }
                Overlay::BundleEditor => self.move_editor_cursor(-1),
                Overlay::ConfirmQuit | Overlay::BundleDelete(_) => {}
            },
            MouseEventKind::ScrollDown => match self.overlay {
                Overlay::None => self.navigate(0, 1),
                Overlay::Logs(_) => self.scroll_logs(3),
                Overlay::Help => self.help_scroll = self.help_scroll.saturating_add(3),
                Overlay::Bundles => {
                    if self.bundle_ui.pane == BundlePane::Members {
                        self.move_bundle_member(1);
                    } else {
                        self.move_bundle_selection(1);
                    }
                }
                Overlay::BundleEditor => self.move_editor_cursor(1),
                Overlay::ConfirmQuit | Overlay::BundleDelete(_) => {}
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
            "Bundles".to_string(),
            "  b                    Open the bundle manager".to_string(),
            "  n / e / d            New / edit / delete a bundle".to_string(),
            "  Enter                Enable or disable selected bundle".to_string(),
            "  Tab                  Switch between bundles and members".to_string(),
            "  o                    Logs for the selected member".to_string(),
            "  Ctrl-S               Save in the bundle editor".to_string(),
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
            format!(
                "  bundles: {}",
                self.bundle_db_path()
                    .map(|path| path.display().to_string())
                    .or_else(|| self.bundle_error.clone())
                    .unwrap_or_else(|| "unavailable".to_string())
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
            bundles: Vec::new(),
            active_bundle_id: None,
            bundle_ui: BundleUiState::default(),
            message: None,
            config_override: None,
            metrics_cli_enabled: true,
            services_dirty: false,
            bundle_store: None,
            bundle_error: None,
            log_return: Overlay::None,
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
