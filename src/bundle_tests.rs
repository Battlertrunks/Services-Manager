//! Behavioral tests for SQLite-backed service bundles and their UI flows.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{App, BundleEditorFocus, BundlePane, MessageKind, Overlay};
use crate::bundles::BundleStore;
use crate::workspace::{CommandSpec, ProjectKind, ServiceSpec, Workspace, WorkspaceOptions};

#[cfg(unix)]
use crate::service::{Service, ServiceStatus};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
const LONG_RUNNING_SCRIPT: &str = "echo 'ready in 5ms'; sleep 30";

fn owned(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

fn service_spec(root: &Path, id: &str, script: &str) -> ServiceSpec {
    ServiceSpec {
        id: id.to_string(),
        name: id.to_string(),
        path_label: id.to_string(),
        path: root.to_path_buf(),
        kind: ProjectKind::Service,
        command: CommandSpec {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
        },
        env: BTreeMap::new(),
        ready_patterns: vec!["ready in".to_string()],
        error_patterns: vec!["error:".to_string()],
        port: None,
        stop_timeout: Duration::from_secs(1),
    }
}

fn workspace(root: &Path, ids: &[&str], script: &str) -> Workspace {
    let specs = ids
        .iter()
        .map(|id| service_spec(root, id, script))
        .collect();
    Workspace {
        root: root.to_path_buf(),
        config_path: None,
        specs,
        warnings: Vec::new(),
        options: WorkspaceOptions {
            metrics_enabled: false,
            ..WorkspaceOptions::default()
        },
    }
}

fn app_with_services(root: &Path, ids: &[&str]) -> App {
    App::new(workspace(root, ids, "true"), None, false)
}

#[cfg(unix)]
fn app_with_processes(root: &Path, ids: &[&str]) -> App {
    App::new(workspace(root, ids, LONG_RUNNING_SCRIPT), None, false)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl_key(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}

fn create_bundle(app: &mut App, name: &str, ids: &[&str]) -> i64 {
    app.open_bundle_editor(None);
    {
        let editor = app.bundle_ui.editor.as_mut().expect("editor should open");
        editor.name = name.to_string();
        editor.selected = ids.iter().map(|id| (*id).to_string()).collect();
    }
    app.save_bundle_editor();
    assert_eq!(app.overlay, Overlay::Bundles, "editor should save {name}");
    app.bundles
        .iter()
        .find(|bundle| bundle.name == name)
        .expect("saved bundle should be reloaded")
        .id
}

fn editor_error(app: &App) -> Option<&str> {
    app.bundle_ui
        .editor
        .as_ref()
        .and_then(|editor| editor.error.as_deref())
}

fn rendered_text(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut text = String::with_capacity((width as usize + 1) * height as usize);
    for y in 0..height {
        for x in 0..width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn render_at_common_sizes(app: &mut App) {
    for (width, height) in [(120, 40), (60, 20), (30, 12), (29, 11), (16, 8)] {
        let _ = rendered_text(app, width, height);
    }
}

#[cfg(unix)]
async fn wait_until(mut condition: impl FnMut() -> bool, description: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {description}");
}

/// Best-effort cleanup so a failed assertion never leaves a child process behind.
#[cfg(unix)]
struct ServiceCleanup(Vec<Arc<Service>>);

#[cfg(unix)]
impl Drop for ServiceCleanup {
    fn drop(&mut self) {
        for service in &self.0 {
            if service.is_running() {
                crate::process::stop_service(service);
            }
        }
    }
}

#[cfg(unix)]
fn service<'a>(app: &'a App, id: &str) -> &'a Arc<Service> {
    let index = app
        .service_index_by_id(id)
        .unwrap_or_else(|| panic!("service {id} should exist"));
    &app.services[index]
}

#[test]
fn store_persists_create_update_and_delete() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp
        .path()
        .join(".services-manager")
        .join("bundles.sqlite3");

    let id = {
        let mut store = BundleStore::open(temp.path()).unwrap();
        assert_eq!(store.path(), db_path.as_path());
        let id = store
            .create("  Alpha  ", &owned(&["beta", "alpha", "beta"]))
            .unwrap();
        let saved = store.get(id).unwrap().expect("created bundle");
        assert_eq!(saved.name, "Alpha");
        assert_eq!(saved.service_ids, owned(&["beta", "alpha"]));
        id
    };
    assert!(db_path.is_file(), "bundle database should live on disk");

    let mut store = BundleStore::open(temp.path()).unwrap();
    let listed = store.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].name, "Alpha");
    assert_eq!(listed[0].service_ids, owned(&["beta", "alpha"]));

    store.update(id, "Renamed", &owned(&["gamma"])).unwrap();
    drop(store);
    let mut store = BundleStore::open(temp.path()).unwrap();
    let updated = store.get(id).unwrap().expect("updated bundle");
    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.service_ids, owned(&["gamma"]));

    store.delete(id).unwrap();
    drop(store);
    let mut store = BundleStore::open(temp.path()).unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(store.delete(id).is_err(), "deleting twice should fail");
}

#[test]
fn store_rejects_invalid_names_and_empty_members() {
    let mut store = BundleStore::open_in_memory().unwrap();

    assert!(store.create("", &owned(&["a"])).is_err());
    assert!(store.create("   ", &owned(&["a"])).is_err());
    assert!(store.create("bad\nname", &owned(&["a"])).is_err());
    assert!(store.create("empty", &[]).is_err());
    assert!(store.create("all-empty-ids", &owned(&["", ""])).is_err());
    assert!(store.list().unwrap().is_empty());

    let max = "x".repeat(60);
    let id = store.create(&max, &owned(&["a"])).unwrap();
    assert_eq!(store.get(id).unwrap().unwrap().name.chars().count(), 60);
    assert!(store.create(&"y".repeat(61), &owned(&["a"])).is_err());
}

#[test]
fn store_enforces_case_insensitive_unique_names() {
    let mut store = BundleStore::open_in_memory().unwrap();
    let alpha = store.create("Alpha", &owned(&["a"])).unwrap();
    assert!(store.create(" alpha ", &owned(&["b"])).is_err());

    let beta = store.create("Beta", &owned(&["b"])).unwrap();
    assert!(store.update(beta, "ALPHA", &owned(&["b"])).is_err());
    store.update(alpha, "ALPHA", &owned(&["c"])).unwrap();
    assert_eq!(store.get(alpha).unwrap().unwrap().name, "ALPHA");
    assert!(store.create("alpha", &owned(&["d"])).is_err());

    let names: Vec<String> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|bundle| bundle.name)
        .collect();
    assert_eq!(names, vec!["ALPHA".to_string(), "Beta".to_string()]);
}

#[test]
fn store_reports_missing_bundles_on_update_and_delete() {
    let mut store = BundleStore::open_in_memory().unwrap();
    let update = store.update(42, "Ghost", &owned(&["a"])).unwrap_err();
    assert!(update.to_string().contains("no longer exists"));
    let delete = store.delete(42).unwrap_err();
    assert!(delete.to_string().contains("no longer exists"));
}

#[test]
fn bundle_editor_rejects_missing_or_duplicate_names_and_empty_selections() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_services(temp.path(), &["alpha", "beta"]);

    app.open_bundle_editor(None);
    app.save_bundle_editor();
    assert_eq!(editor_error(&app), Some("Bundle name is required"));

    app.bundle_ui.editor.as_mut().unwrap().name = "   ".to_string();
    app.save_bundle_editor();
    assert_eq!(editor_error(&app), Some("Bundle name is required"));

    app.bundle_ui.editor.as_mut().unwrap().name = "x".repeat(61);
    app.save_bundle_editor();
    assert!(
        editor_error(&app)
            .unwrap()
            .contains("60 characters or fewer")
    );

    app.bundle_ui.editor.as_mut().unwrap().name = "Alpha".to_string();
    app.save_bundle_editor();
    assert!(
        editor_error(&app)
            .unwrap()
            .contains("Select at least one available service")
    );

    app.bundle_ui
        .editor
        .as_mut()
        .unwrap()
        .selected
        .insert("alpha".to_string());
    app.save_bundle_editor();
    assert_eq!(app.overlay, Overlay::Bundles);
    assert_eq!(app.bundles.len(), 1);
    assert_eq!(app.bundles[0].name, "Alpha");
    assert_eq!(app.bundles[0].service_ids, owned(&["alpha"]));

    app.open_bundle_editor(None);
    {
        let editor = app.bundle_ui.editor.as_mut().unwrap();
        editor.name = "  aLpHa ".to_string();
        editor.selected.insert("beta".to_string());
    }
    app.save_bundle_editor();
    assert_eq!(app.overlay, Overlay::BundleEditor);
    assert!(editor_error(&app).unwrap().contains("already exists"));
    assert_eq!(app.bundles.len(), 1, "duplicate must not be persisted");
}

#[test]
fn bundle_editor_keyboard_flow_creates_and_cancels_bundles() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_services(temp.path(), &["alpha", "beta"]);

    app.open_bundles();
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.overlay, Overlay::BundleEditor);
    for ch in "Keyboard".chars() {
        app.handle_key(key(KeyCode::Char(ch)));
    }
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(
        app.bundle_ui.editor.as_ref().unwrap().focus,
        BundleEditorFocus::Services
    );
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(ctrl_key('s'));

    assert_eq!(app.overlay, Overlay::Bundles);
    assert_eq!(app.bundles.len(), 1);
    assert_eq!(app.bundles[0].name, "Keyboard");
    assert_eq!(app.bundles[0].service_ids, owned(&["alpha"]));

    app.handle_key(key(KeyCode::Char('n')));
    app.handle_key(key(KeyCode::Char('X')));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, Overlay::Bundles);
    assert!(app.bundle_ui.editor.is_none());
    assert_eq!(app.bundles.len(), 1, "cancelled editor must not save");
}

#[test]
fn bundle_editor_updates_existing_definitions() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_services(temp.path(), &["alpha", "beta"]);
    let id = create_bundle(&mut app, "Original", &["alpha"]);

    app.open_bundle_editor(Some(id));
    {
        let editor = app.bundle_ui.editor.as_ref().unwrap();
        assert_eq!(editor.name, "Original");
        assert!(editor.selected.contains("alpha"));
    }
    {
        let editor = app.bundle_ui.editor.as_mut().unwrap();
        editor.name = "Renamed".to_string();
        editor.selected.clear();
        editor.selected.insert("beta".to_string());
    }
    app.save_bundle_editor();

    assert_eq!(app.overlay, Overlay::Bundles);
    assert_eq!(app.bundles.len(), 1);
    assert_eq!(app.bundles[0].id, id);
    assert_eq!(app.bundles[0].name, "Renamed");
    assert_eq!(app.bundles[0].service_ids, owned(&["beta"]));
}

#[test]
fn editing_a_bundle_drops_missing_services_on_save() {
    let temp = tempfile::tempdir().unwrap();
    {
        let mut store = BundleStore::open(temp.path()).unwrap();
        store
            .create("Legacy", &owned(&["ghost-a", "alpha", "ghost-b"]))
            .unwrap();
    }

    let mut app = app_with_services(temp.path(), &["alpha", "beta"]);
    assert_eq!(app.bundles.len(), 1);
    assert_eq!(app.bundle_status_counts(&app.bundles[0]), (0, 0, 0, 1, 2));
    let id = app.bundles[0].id;

    app.open_bundle_editor(Some(id));
    assert_eq!(
        app.bundle_ui.editor.as_ref().unwrap().missing_services,
        2,
        "editor should count unavailable members"
    );
    {
        let editor = app.bundle_ui.editor.as_mut().unwrap();
        editor.selected.clear();
        editor.selected.insert("ghost-a".to_string());
    }
    app.save_bundle_editor();
    assert_eq!(
        editor_error(&app),
        Some("Select at least one available service")
    );
    assert_eq!(
        app.bundles[0].service_ids.len(),
        3,
        "failed save is a no-op"
    );

    app.bundle_ui
        .editor
        .as_mut()
        .unwrap()
        .selected
        .insert("alpha".to_string());
    app.save_bundle_editor();

    assert_eq!(app.overlay, Overlay::Bundles);
    let saved = app.bundles.iter().find(|bundle| bundle.id == id).unwrap();
    assert_eq!(saved.name, "Legacy");
    assert_eq!(saved.service_ids, owned(&["alpha"]));
    assert_eq!(app.bundle_status_counts(saved), (0, 0, 0, 1, 0));
}

#[test]
fn active_bundle_cannot_be_edited_until_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_services(temp.path(), &["alpha"]);
    let id = create_bundle(&mut app, "Active", &["alpha"]);

    app.active_bundle_id = Some(id);
    app.open_bundles();
    app.handle_key(key(KeyCode::Char('e')));
    assert_eq!(app.overlay, Overlay::Bundles);
    assert!(app.bundle_ui.editor.is_none());
    assert!(
        app.bundle_ui
            .notice
            .as_deref()
            .unwrap()
            .contains("Disable the bundle before editing")
    );

    app.active_bundle_id = None;
    app.handle_key(key(KeyCode::Char('e')));
    assert_eq!(app.overlay, Overlay::BundleEditor);
    assert!(app.bundle_ui.editor.is_some());
}

#[test]
fn enabling_a_bundle_with_only_missing_services_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    {
        let mut store = BundleStore::open(temp.path()).unwrap();
        store
            .create("Ghosts", &owned(&["ghost-a", "ghost-b"]))
            .unwrap();
    }
    let mut app = app_with_services(temp.path(), &["alpha"]);
    let id = app.bundles[0].id;

    app.enable_bundle(id);

    assert_eq!(app.active_bundle_id, None);
    assert!(!app.services[0].is_running());
    assert!(
        app.bundle_ui
            .notice
            .as_deref()
            .unwrap()
            .contains("none of its services are available")
    );
    assert_eq!(app.current_message().unwrap().kind, MessageKind::Error);
    assert_eq!(app.bundle_status_counts(&app.bundles[0]), (0, 0, 0, 0, 2));
}

#[test]
fn member_logs_return_to_bundle_manager() {
    let temp = tempfile::tempdir().unwrap();
    {
        let mut store = BundleStore::open(temp.path()).unwrap();
        store.create("Mixed", &owned(&["alpha", "ghost"])).unwrap();
    }
    let mut app = app_with_services(temp.path(), &["alpha"]);
    app.open_bundles();
    app.bundle_ui.pane = BundlePane::Members;

    app.bundle_ui.member_selected = 1;
    app.handle_key(key(KeyCode::Char('o')));
    assert_eq!(app.overlay, Overlay::Bundles);
    assert!(
        app.bundle_ui
            .notice
            .as_deref()
            .unwrap()
            .contains("no longer available")
    );

    app.bundle_ui.member_selected = 0;
    app.handle_key(key(KeyCode::Char('o')));
    assert_eq!(app.overlay, Overlay::Logs(0));
    assert_eq!(app.selected, 0);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, Overlay::Bundles);

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.overlay, Overlay::Logs(0));
    app.handle_key(key(KeyCode::Char('q')));
    assert_eq!(app.overlay, Overlay::Bundles);

    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, Overlay::None);
}

#[test]
fn regular_grid_logs_return_to_the_grid() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_services(temp.path(), &["alpha"]);

    app.handle_key(key(KeyCode::Char('o')));
    assert_eq!(app.overlay, Overlay::Logs(0));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, Overlay::None);
}

#[test]
fn bundle_definitions_persist_but_active_state_is_not_restored() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp
        .path()
        .join(".services-manager")
        .join("bundles.sqlite3");
    let id = {
        let mut app = app_with_services(temp.path(), &["alpha", "beta"]);
        assert_eq!(app.bundle_db_path(), Some(db_path.as_path()));
        let id = create_bundle(&mut app, "Persisted", &["alpha", "beta"]);
        app.active_bundle_id = Some(id);
        id
    };

    let app = app_with_services(temp.path(), &["alpha", "beta"]);
    assert_eq!(app.active_bundle_id, None, "active state is session-only");
    assert_eq!(app.bundles.len(), 1);
    assert_eq!(app.bundles[0].id, id);
    assert_eq!(app.bundles[0].name, "Persisted");
    assert_eq!(app.bundles[0].service_ids, owned(&["alpha", "beta"]));
    assert!(app.bundle_error().is_none());
}

#[test]
fn workspace_startup_loads_services_and_saved_bundles() {
    let temp = tempfile::tempdir().unwrap();
    let app_dir = temp.path().join("apps").join("alpha");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(
        app_dir.join("package.json"),
        r#"{"name":"alpha","scripts":{"dev":"vite"},"dependencies":{"vue":"^3"}}"#,
    )
    .unwrap();
    {
        let mut store = BundleStore::open(temp.path()).unwrap();
        store.create("Existing", &owned(&["apps/alpha"])).unwrap();
    }

    let workspace = Workspace::load(temp.path(), None, false).unwrap();
    assert_eq!(workspace.specs.len(), 1);
    let app = App::new(workspace, None, false);

    assert_eq!(app.services.len(), 1);
    assert_eq!(app.bundles.len(), 1);
    assert_eq!(app.bundles[0].service_ids, owned(&["apps/alpha"]));
    assert_eq!(app.active_bundle_id, None);
    assert!(app.bundle_error().is_none());
    assert!(app.warnings.is_empty());
    assert!(
        temp.path()
            .join(".services-manager")
            .join("bundles.sqlite3")
            .is_file()
    );
}

#[test]
fn bundle_storage_failure_does_not_block_startup() {
    let temp = tempfile::tempdir().unwrap();
    let app_dir = temp.path().join("apps").join("alpha");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(
        app_dir.join("package.json"),
        r#"{"name":"alpha","scripts":{"dev":"vite"},"dependencies":{"vue":"^3"}}"#,
    )
    .unwrap();
    std::fs::write(temp.path().join(".services-manager"), b"not a directory").unwrap();

    let workspace = Workspace::load(temp.path(), None, false).unwrap();
    let mut app = App::new(workspace, None, false);

    assert_eq!(app.services.len(), 1);
    assert!(app.bundle_error().is_some());
    assert!(app.bundle_db_path().is_none());
    assert!(
        app.warnings
            .iter()
            .any(|warning| warning.contains("Bundles unavailable"))
    );

    app.open_bundles();
    assert_eq!(app.overlay, Overlay::Bundles);
    assert!(app.bundle_ui.notice.is_some());
    app.open_bundle_editor(None);
    assert!(app.bundle_ui.editor.is_none());
    assert!(app.bundle_ui.notice.is_some());
}

#[test]
fn bundle_manager_renders_active_and_missing_members() {
    let temp = tempfile::tempdir().unwrap();
    {
        let mut store = BundleStore::open(temp.path()).unwrap();
        store
            .create("Alpha bundle", &owned(&["alpha", "ghost"]))
            .unwrap();
        store.create("Beta bundle", &owned(&["beta"])).unwrap();
    }
    let mut app = app_with_services(temp.path(), &["alpha", "beta"]);
    let alpha_id = app
        .bundles
        .iter()
        .find(|bundle| bundle.name == "Alpha bundle")
        .unwrap()
        .id;
    app.open_bundles();
    app.active_bundle_id = Some(alpha_id);

    let text = rendered_text(&mut app, 120, 40);
    assert!(text.contains("Bundles"));
    assert!(text.contains("Alpha bundle"));
    assert!(text.contains("Beta bundle"));
    assert!(text.contains("ENABLED"));
    assert!(text.contains("missing"));
    assert!(text.contains("ghost"));

    app.bundle_ui.pane = BundlePane::Members;
    app.bundle_ui.member_selected = 1;
    let text = rendered_text(&mut app, 80, 24);
    assert!(text.contains("missing"));
    assert!(text.contains("ghost"));

    render_at_common_sizes(&mut app);
}

#[test]
fn bundle_editor_and_delete_overlays_render() {
    let temp = tempfile::tempdir().unwrap();
    {
        let mut store = BundleStore::open(temp.path()).unwrap();
        store.create("Alpha bundle", &owned(&["alpha"])).unwrap();
        store.create("Beta bundle", &owned(&["beta"])).unwrap();
    }
    let mut app = app_with_services(temp.path(), &["alpha", "beta"]);
    let alpha_id = app
        .bundles
        .iter()
        .find(|bundle| bundle.name == "Alpha bundle")
        .unwrap()
        .id;
    app.open_bundles();

    app.open_bundle_editor(Some(alpha_id));
    let text = rendered_text(&mut app, 120, 40);
    assert!(text.contains("Edit bundle"));
    assert!(text.contains("Alpha bundle"));

    app.bundle_ui.editor.as_mut().unwrap().name = "Beta bundle".to_string();
    app.save_bundle_editor();
    assert_eq!(app.overlay, Overlay::BundleEditor);
    let text = rendered_text(&mut app, 80, 24);
    assert!(text.contains("already exists"));

    app.handle_key(key(KeyCode::Esc));
    app.open_bundle_editor(None);
    let text = rendered_text(&mut app, 80, 24);
    assert!(text.contains("New bundle"));
    app.handle_key(key(KeyCode::Esc));

    app.overlay = Overlay::BundleDelete(alpha_id);
    let text = rendered_text(&mut app, 120, 40);
    assert!(text.contains("Delete bundle"));
    assert!(text.contains("Alpha bundle"));
    assert!(text.contains("cannot be undone"));

    app.active_bundle_id = Some(alpha_id);
    let text = rendered_text(&mut app, 120, 40);
    assert!(text.contains("enabled"));

    render_at_common_sizes(&mut app);

    app.overlay = Overlay::BundleDelete(9999);
    let _ = rendered_text(&mut app, 80, 24);
}

#[test]
fn bundle_manager_renders_empty_state() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_services(temp.path(), &["alpha"]);
    app.open_bundles();

    let text = rendered_text(&mut app, 120, 40);
    assert!(text.contains("No bundles yet"));
    render_at_common_sizes(&mut app);
}

#[test]
fn dashboard_renders_port_on_its_own_row() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_services(temp.path(), &["alpha"]);
    app.metrics_enabled = true;
    let service = app.services[0].clone();
    service.runtime.begin_start(Some(5173)).unwrap();
    let run_id = service
        .runtime
        .install_run(
            42,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();
    service
        .runtime
        .set_metrics_for(run_id, 42, 100.0, 1024 * 1024 * 1024);

    let text = rendered_text(&mut app, 60, 20);
    let rows: Vec<&str> = text.lines().collect();
    let metrics_row = rows
        .iter()
        .position(|row| row.contains("cpu 100%") && row.contains("mem 1.0G"))
        .expect("metrics row should render");
    assert!(
        !rows[metrics_row].contains(":5173"),
        "port should not share the metrics row: {}",
        rows[metrics_row]
    );
    assert!(
        rows.get(metrics_row + 1)
            .is_some_and(|row| row.contains(":5173")),
        "port should render on its own row directly below metrics"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn only_one_bundle_is_active_and_shared_services_keep_running() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_processes(temp.path(), &["shared", "alpha", "beta"]);
    let _cleanup = ServiceCleanup(app.services.clone());
    let first = create_bundle(&mut app, "First", &["shared", "alpha"]);
    let second = create_bundle(&mut app, "Second", &["shared", "beta"]);
    let shared = Arc::clone(service(&app, "shared"));
    let alpha = Arc::clone(service(&app, "alpha"));
    let beta = Arc::clone(service(&app, "beta"));

    app.enable_bundle(first);
    assert_eq!(app.active_bundle_id, Some(first));
    wait_until(
        || shared.is_running() && alpha.is_running(),
        "the first bundle's services to start",
    )
    .await;
    let shared_run = shared
        .runtime
        .active_run()
        .expect("shared service should have a run")
        .run_id;

    app.enable_bundle(second);
    assert_eq!(app.active_bundle_id, Some(second));
    wait_until(
        || !alpha.is_running(),
        "the first bundle's exclusive service to stop",
    )
    .await;
    wait_until(
        || shared.is_running() && beta.is_running(),
        "the second bundle's services to run",
    )
    .await;
    assert_eq!(
        shared
            .runtime
            .active_run()
            .expect("shared service should still have a run")
            .run_id,
        shared_run,
        "a service shared by both bundles must keep its existing process"
    );

    app.disable_active_bundle();
    assert_eq!(app.active_bundle_id, None);
    wait_until(
        || !shared.is_running() && !beta.is_running(),
        "disabling the second bundle to stop its services",
    )
    .await;
    assert!(!alpha.is_running());
}

#[cfg(unix)]
#[tokio::test]
async fn deleting_an_active_bundle_stops_services_and_clears_state() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_processes(temp.path(), &["alpha", "beta"]);
    let _cleanup = ServiceCleanup(app.services.clone());
    let id = create_bundle(&mut app, "Active", &["alpha", "beta"]);
    let alpha = Arc::clone(service(&app, "alpha"));
    let beta = Arc::clone(service(&app, "beta"));

    app.enable_bundle(id);
    assert_eq!(app.active_bundle_id, Some(id));
    wait_until(
        || alpha.is_running() && beta.is_running(),
        "the active bundle's services to start",
    )
    .await;

    app.delete_bundle(id);

    assert_eq!(app.active_bundle_id, None);
    assert!(app.bundles.is_empty());
    assert_eq!(app.overlay, Overlay::Bundles);
    assert!(app.bundle_ui.notice.as_deref().unwrap().contains("Deleted"));
    wait_until(
        || !alpha.is_running() && !beta.is_running(),
        "the deleted bundle's services to stop",
    )
    .await;

    let store = BundleStore::open(temp.path()).unwrap();
    assert!(store.list().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn manual_grid_start_and_stop_work_while_bundles_exist() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = app_with_processes(temp.path(), &["alpha", "beta"]);
    let _cleanup = ServiceCleanup(app.services.clone());
    let id = create_bundle(&mut app, "Saved", &["alpha"]);
    let alpha = Arc::clone(service(&app, "alpha"));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, Overlay::None);

    app.handle_key(key(KeyCode::Enter));
    wait_until(|| alpha.is_running(), "manual start from the grid").await;
    assert_eq!(app.active_bundle_id, None);

    app.handle_key(key(KeyCode::Enter));
    wait_until(|| !alpha.is_running(), "manual stop from the grid").await;
    assert_eq!(app.bundles.len(), 1);
    assert_eq!(app.bundles[0].id, id);
    assert_eq!(app.active_bundle_id, None);
    assert_eq!(alpha.status(), ServiceStatus::Stopped);
}
