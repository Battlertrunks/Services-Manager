//! Workspace discovery and configuration.
//!
//! A workspace is a directory containing Node projects (MFEs and micro-services). By default we
//! walk a couple of levels down looking for `package.json` files with a runnable script. An
//! optional `services-manager.toml` can define services explicitly.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const CONFIG_FILE_NAMES: [&str; 2] = ["services-manager.toml", ".services-manager.toml"];

pub const DEFAULT_SCAN_DEPTH: usize = 2;
pub const DEFAULT_READY_TIMEOUT_SECS: u64 = 8;
pub const DEFAULT_STOP_TIMEOUT_SECS: u64 = 5;
pub const DEFAULT_LOG_BUFFER_LINES: usize = 2_000;
pub const DEFAULT_METRICS_INTERVAL_MS: u64 = 2_000;

/// Directories that never contain projects we want to manage.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".nuxt",
    ".output",
    ".turbo",
    ".cache",
    ".parcel-cache",
    ".venv",
    "venv",
    "__pycache__",
    ".idea",
    ".vscode",
];

/// Common readiness messages emitted by JS dev servers and frameworks.
const DEFAULT_READY_PATTERNS: &[&str] = &[
    "ready in",
    "ready on",
    "compiled successfully",
    "webpack compiled",
    "app running at",
    "dev server running",
    "localhost:",
    "127.0.0.1:",
    "listening on",
    "listening at",
    "server listening",
    "server running",
    "server started",
    "server is running",
    "started server",
    "successfully started",
    "serving at",
    "running at http",
    "local:",
];

/// Messages that mean something went wrong. These intentionally avoid a bare "error" match so
/// that lines like "0 errors" do not paint a healthy service red.
const DEFAULT_ERROR_PATTERNS: &[&str] = &[
    "npm err!",
    "eaddrinuse",
    "address already in use",
    "failed to compile",
    "failed to resolve",
    "compilation failed",
    "could not resolve",
    "cannot find module",
    "module not found",
    "unhandledpromiserejection",
    "error:",
    " error ",
    "typeerror",
    "referenceerror",
    "syntaxerror",
    "unhandled rejection",
    "uncaught exception",
    "fatal error",
    "failed to start",
    "error in",
    "[error]",
    "✖",
    "✗",
    "✘",
];

const BENIGN_ERROR_LINES: &[&str] = &[
    "0 errors",
    "0 error",
    "no errors",
    "no error",
    "without errors",
    "error-free",
    "errors: 0",
    "errors 0",
    "0 failed",
    "0 failures",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    #[serde(alias = "frontend")]
    Mfe,
    #[serde(alias = "backend", alias = "api")]
    Service,
    App,
}

impl ProjectKind {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Mfe => "MFE",
            Self::Service => "SVC",
            Self::App => "APP",
        }
    }

    fn sort_rank(self) -> u8 {
        match self {
            Self::Mfe => 0,
            Self::Service => 1,
            Self::App => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn npm_script(script: &str) -> Self {
        if script == "start" {
            Self {
                program: "npm".to_string(),
                args: vec!["start".to_string()],
            }
        } else {
            Self {
                program: "npm".to_string(),
                args: vec!["run".to_string(), script.to_string()],
            }
        }
    }

    pub fn display(&self) -> String {
        let mut out = quote_arg(&self.program);
        for arg in &self.args {
            out.push(' ');
            out.push_str(&quote_arg(arg));
        }
        out
    }
}

fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '='))
    {
        arg.to_string()
    } else {
        format!("'{arg}'")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Stable identifier, usually the path relative to the workspace root.
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub kind: ProjectKind,
    pub command: CommandSpec,
    pub env: BTreeMap<String, String>,
    pub ready_patterns: Vec<String>,
    pub error_patterns: Vec<String>,
    pub port: Option<u16>,
    pub stop_timeout: Duration,
}

impl ServiceSpec {
    pub fn relative_path(&self) -> String {
        self.id.clone()
    }

    pub fn line_is_error(&self, line_lower: &str) -> bool {
        if BENIGN_ERROR_LINES.iter().any(|p| line_lower.contains(p)) {
            return false;
        }
        self.error_patterns.iter().any(|p| line_lower.contains(p))
    }

    pub fn line_is_ready(&self, line_lower: &str) -> bool {
        self.ready_patterns.iter().any(|p| line_lower.contains(p))
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceOptions {
    pub scan_depth: usize,
    pub ready_timeout: Duration,
    pub stop_timeout: Duration,
    pub log_capacity: usize,
    pub metrics_interval: Duration,
    pub metrics_enabled: bool,
}

impl Default for WorkspaceOptions {
    fn default() -> Self {
        Self {
            scan_depth: DEFAULT_SCAN_DEPTH,
            ready_timeout: Duration::from_secs(DEFAULT_READY_TIMEOUT_SECS),
            stop_timeout: Duration::from_secs(DEFAULT_STOP_TIMEOUT_SECS),
            log_capacity: DEFAULT_LOG_BUFFER_LINES,
            metrics_interval: Duration::from_millis(DEFAULT_METRICS_INTERVAL_MS),
            metrics_enabled: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub specs: Vec<ServiceSpec>,
    pub warnings: Vec<String>,
    pub options: WorkspaceOptions,
}

impl Workspace {
    pub fn load(
        root: &Path,
        config_override: Option<&Path>,
        metrics_enabled: bool,
    ) -> Result<Self> {
        if !root.is_dir() {
            bail!("workspace root is not a directory: {}", root.display());
        }
        let root = fs::canonicalize(root)
            .with_context(|| format!("cannot resolve workspace root {}", root.display()))?;

        let config_path = locate_config(&root, config_override)?;
        let file_config = match &config_path {
            Some(path) => {
                let raw = fs::read_to_string(path)
                    .with_context(|| format!("cannot read config {}", path.display()))?;
                Some(
                    toml::from_str::<FileConfig>(&raw)
                        .with_context(|| format!("invalid config {}", path.display()))?,
                )
            }
            None => None,
        };

        let mut options = WorkspaceOptions::default();
        if let Some(config) = &file_config {
            options.apply_file(&config.workspace);
        }
        options.metrics_enabled &= metrics_enabled;

        let mut warnings = Vec::new();
        let mut specs = Vec::new();

        let configured_services = file_config
            .as_ref()
            .map(|config| !config.services.is_empty())
            .unwrap_or(false);

        if configured_services && let Some(config) = &file_config {
            for (index, entry) in config.services.iter().enumerate() {
                match build_configured_service(&root, entry, &options, &mut warnings) {
                    Ok(spec) => specs.push(spec),
                    Err(message) => {
                        warnings.push(format!("services[{index}]: {message}"));
                    }
                }
            }
        }

        if !configured_services {
            specs.extend(discover_services(
                &root,
                options.scan_depth,
                options.stop_timeout,
                &mut warnings,
            ));

            if specs.is_empty()
                && let Ok(Some(spec)) = build_root_service(&root, &options, &mut warnings)
            {
                specs.push(spec);
                warnings.push(
                    "No child projects found; loaded the current directory as one project."
                        .to_string(),
                );
            }
        }

        dedupe_services(&mut specs, &mut warnings);
        specs.sort_by(|a, b| {
            a.kind
                .sort_rank()
                .cmp(&b.kind.sort_rank())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });

        if specs.is_empty() {
            warnings.push(format!(
                "No runnable Node projects found under {}. Add a package.json with a dev, start, or serve script.",
                root.display()
            ));
        }

        Ok(Self {
            root,
            config_path,
            specs,
            warnings,
            options,
        })
    }
}

fn locate_config(root: &Path, config_override: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(path) = config_override {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .context("cannot resolve current directory")?
                .join(path)
        };
        let path = fs::canonicalize(&path)
            .with_context(|| format!("config file not found: {}", path.display()))?;
        if !path.is_file() {
            bail!("config path is not a file: {}", path.display());
        }
        return Ok(Some(path));
    }

    for name in CONFIG_FILE_NAMES {
        let candidate = root.join(name);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    workspace: FileWorkspace,
    #[serde(default)]
    services: Vec<FileService>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWorkspace {
    scan_depth: Option<usize>,
    ready_timeout_secs: Option<u64>,
    stop_timeout_secs: Option<u64>,
    log_buffer_lines: Option<usize>,
    metrics_interval_ms: Option<u64>,
    metrics: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileService {
    name: Option<String>,
    path: String,
    #[serde(alias = "type")]
    kind: Option<ProjectKind>,
    start: Option<StartCommand>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    ready_patterns: Vec<String>,
    #[serde(default)]
    error_patterns: Vec<String>,
    port: Option<u16>,
    stop_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum StartCommand {
    Line(String),
    Argv(Vec<String>),
}

impl StartCommand {
    fn into_spec(self) -> std::result::Result<CommandSpec, String> {
        match self {
            Self::Line(line) => {
                let parts = shell_words::split(&line)
                    .map_err(|err| format!("cannot parse start command: {err}"))?;
                if parts.is_empty() {
                    return Err("start command is empty".to_string());
                }
                let mut parts = parts.into_iter();
                Ok(CommandSpec {
                    program: parts.next().unwrap_or_default(),
                    args: parts.collect(),
                })
            }
            Self::Argv(argv) => {
                let mut argv = argv.into_iter();
                let program = argv
                    .next()
                    .filter(|part| !part.is_empty())
                    .ok_or_else(|| "start command is empty".to_string())?;
                Ok(CommandSpec {
                    program,
                    args: argv.collect(),
                })
            }
        }
    }
}

impl WorkspaceOptions {
    fn apply_file(&mut self, file: &FileWorkspace) {
        if let Some(depth) = file.scan_depth {
            self.scan_depth = depth.clamp(1, 6);
        }
        if let Some(secs) = file.ready_timeout_secs {
            self.ready_timeout = Duration::from_secs(secs.clamp(1, 120));
        }
        if let Some(secs) = file.stop_timeout_secs {
            self.stop_timeout = Duration::from_secs(secs.clamp(1, 60));
        }
        if let Some(lines) = file.log_buffer_lines {
            self.log_capacity = lines.clamp(100, 100_000);
        }
        if let Some(ms) = file.metrics_interval_ms {
            self.metrics_interval = Duration::from_millis(ms.clamp(500, 60_000));
        }
        if let Some(enabled) = file.metrics {
            self.metrics_enabled &= enabled;
        }
    }
}

fn build_configured_service(
    root: &Path,
    entry: &FileService,
    options: &WorkspaceOptions,
    warnings: &mut Vec<String>,
) -> std::result::Result<ServiceSpec, String> {
    let raw_path = Path::new(&entry.path);
    let path = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        root.join(raw_path)
    };
    let path = fs::canonicalize(&path)
        .map_err(|err| format!("{}: path is not accessible: {err}", entry.path))?;
    if !path.is_dir() {
        return Err(format!("{}: path is not a directory", entry.path));
    }

    let package = read_package_json(&path, warnings).ok().flatten();
    let command = match &entry.start {
        Some(start) => start
            .clone()
            .into_spec()
            .map_err(|err| format!("{}: {err}", entry.path))?,
        None => {
            let package = package.as_ref().ok_or_else(|| {
                format!(
                    "{}: no package.json found; set a start command explicitly",
                    entry.path
                )
            })?;
            let script = pick_start_script(&package.scripts)
                .ok_or_else(|| format!("{}: no dev, start, or serve script", entry.path))?;
            CommandSpec::npm_script(&script)
        }
    };

    let name = entry
        .name
        .clone()
        .or_else(|| package.as_ref().and_then(|pkg| pkg.name.clone()))
        .unwrap_or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "service".to_string())
        });

    let kind = entry.kind.unwrap_or_else(|| {
        package
            .as_ref()
            .map(|package| infer_kind(&name, &package.all_deps(), &package.scripts))
            .unwrap_or(ProjectKind::App)
    });

    Ok(ServiceSpec {
        id: relative_id(root, &path),
        name,
        path,
        kind,
        command,
        env: entry.env.clone(),
        ready_patterns: merge_patterns(DEFAULT_READY_PATTERNS, &entry.ready_patterns),
        error_patterns: merge_patterns(DEFAULT_ERROR_PATTERNS, &entry.error_patterns),
        port: entry.port.filter(|port| *port > 0),
        stop_timeout: entry
            .stop_timeout_secs
            .map(|secs| Duration::from_secs(secs.clamp(1, 60)))
            .unwrap_or(options.stop_timeout),
    })
}

fn build_root_service(
    root: &Path,
    options: &WorkspaceOptions,
    warnings: &mut Vec<String>,
) -> Result<Option<ServiceSpec>> {
    if !root.join("package.json").is_file() {
        return Ok(None);
    }
    let Some(package) = read_package_json(root, warnings)? else {
        return Ok(None);
    };
    let Some(script) = pick_start_script(&package.scripts) else {
        return Ok(None);
    };
    let name = package
        .name
        .clone()
        .or_else(|| {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "service".to_string());
    let kind = infer_kind(&name, &package.all_deps(), &package.scripts);
    Ok(Some(ServiceSpec {
        id: ".".to_string(),
        name,
        path: root.to_path_buf(),
        kind,
        command: CommandSpec::npm_script(&script),
        env: BTreeMap::new(),
        ready_patterns: merge_patterns(DEFAULT_READY_PATTERNS, &[]),
        error_patterns: merge_patterns(DEFAULT_ERROR_PATTERNS, &[]),
        port: None,
        stop_timeout: options.stop_timeout,
    }))
}

#[derive(Debug, Default)]
struct PackageInfo {
    name: Option<String>,
    scripts: BTreeMap<String, String>,
    dependencies: BTreeMap<String, serde_json::Value>,
    dev_dependencies: BTreeMap<String, serde_json::Value>,
    peer_dependencies: BTreeMap<String, serde_json::Value>,
}

impl PackageInfo {
    fn all_deps(&self) -> Vec<String> {
        self.dependencies
            .keys()
            .chain(self.dev_dependencies.keys())
            .chain(self.peer_dependencies.keys())
            .map(|dep| dep.to_lowercase())
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct RawPackageJson {
    name: Option<String>,
    #[serde(default)]
    scripts: BTreeMap<String, String>,
    #[serde(default)]
    dependencies: BTreeMap<String, serde_json::Value>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, serde_json::Value>,
    #[serde(default, rename = "peerDependencies")]
    peer_dependencies: BTreeMap<String, serde_json::Value>,
}

fn read_package_json(dir: &Path, warnings: &mut Vec<String>) -> Result<Option<PackageInfo>> {
    let path = dir.join("package.json");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            warnings.push(format!(
                "{}: cannot read package.json: {err}",
                path.display()
            ));
            return Ok(None);
        }
    };
    match serde_json::from_str::<RawPackageJson>(&raw) {
        Ok(package) => Ok(Some(PackageInfo {
            name: package.name,
            scripts: package.scripts,
            dependencies: package.dependencies,
            dev_dependencies: package.dev_dependencies,
            peer_dependencies: package.peer_dependencies,
        })),
        Err(err) => {
            warnings.push(format!("{}: invalid package.json: {err}", path.display()));
            Ok(None)
        }
    }
}

fn discover_services(
    root: &Path,
    scan_depth: usize,
    stop_timeout: Duration,
    warnings: &mut Vec<String>,
) -> Vec<ServiceSpec> {
    let mut specs = Vec::new();
    let mut visited = HashSet::new();
    scan_dir(
        root,
        root,
        0,
        scan_depth,
        stop_timeout,
        &mut specs,
        warnings,
        &mut visited,
    );
    specs
}

#[allow(clippy::too_many_arguments)]
fn scan_dir(
    root: &Path,
    dir: &Path,
    depth: usize,
    scan_depth: usize,
    stop_timeout: Duration,
    specs: &mut Vec<ServiceSpec>,
    warnings: &mut Vec<String>,
    visited: &mut HashSet<PathBuf>,
) {
    if depth >= scan_depth {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                return None;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') || IGNORED_DIRS.contains(&name_str.as_ref()) {
                return None;
            }
            Some(entry.path())
        })
        .collect();
    dirs.sort();

    for child in dirs {
        if !visited.insert(child.clone()) {
            continue;
        }

        if child.join("package.json").is_file()
            && let Some(spec) = build_discovered_service(root, &child, stop_timeout, warnings)
        {
            specs.push(spec);
            // A runnable package is a project boundary. Do not wander into its own subpackages.
            continue;
        }

        scan_dir(
            root,
            &child,
            depth + 1,
            scan_depth,
            stop_timeout,
            specs,
            warnings,
            visited,
        );
    }
}

fn build_discovered_service(
    root: &Path,
    dir: &Path,
    stop_timeout: Duration,
    warnings: &mut Vec<String>,
) -> Option<ServiceSpec> {
    let package = match read_package_json(dir, warnings) {
        Ok(Some(package)) => package,
        Ok(None) => return None,
        Err(_) => return None,
    };
    let Some(script) = pick_start_script(&package.scripts) else {
        warnings.push(format!(
            "{}: package.json has no dev, start, or serve script (skipped)",
            dir.display()
        ));
        return None;
    };

    let name = package
        .name
        .clone()
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "service".to_string());
    let kind = infer_kind(&name, &package.all_deps(), &package.scripts);

    Some(ServiceSpec {
        id: relative_id(root, dir),
        name,
        path: dir.to_path_buf(),
        kind,
        command: CommandSpec::npm_script(&script),
        env: BTreeMap::new(),
        ready_patterns: merge_patterns(DEFAULT_READY_PATTERNS, &[]),
        error_patterns: merge_patterns(DEFAULT_ERROR_PATTERNS, &[]),
        port: None,
        stop_timeout,
    })
}

/// Picks the most useful development script from a package.json.
pub fn pick_start_script(scripts: &BTreeMap<String, String>) -> Option<String> {
    const PREFERRED: &[&str] = &[
        "dev",
        "start",
        "serve",
        "develop",
        "start:dev",
        "dev:server",
        "start:server",
        "serve:dev",
        "watch",
    ];

    for name in PREFERRED {
        if scripts.contains_key(*name) {
            return Some((*name).to_string());
        }
    }

    // Monorepos often use names like `dev:web` or `start:api`.
    for prefix in ["dev:", "start:", "serve:"] {
        if let Some((name, _)) = scripts
            .iter()
            .filter(|(name, _)| name.starts_with(prefix))
            .min_by_key(|(name, _)| name.len())
        {
            return Some(name.clone());
        }
    }

    None
}

/// Guesses whether a project is a frontend MFE, a backend service, or something else.
pub fn infer_kind(
    name: &str,
    dependencies: &[String],
    scripts: &BTreeMap<String, String>,
) -> ProjectKind {
    let frontend = [
        "vue",
        "react",
        "react-dom",
        "next",
        "nuxt",
        "svelte",
        "@sveltejs/kit",
        "solid-js",
        "preact",
        "astro",
        "vite",
        "@vitejs/plugin-vue",
        "@vitejs/plugin-react",
        "react-scripts",
        "@angular/core",
        "@vue/cli-service",
        "vue-router",
        "react-router",
        "react-router-dom",
        "tailwindcss",
    ];
    let backend = [
        "express",
        "fastify",
        "@fastify/core",
        "koa",
        "@nestjs/core",
        "@hapi/hapi",
        "hapi",
        "hono",
        "elysia",
        "restify",
        "apollo-server",
        "@apollo/server",
        "graphql-yoga",
        "moleculer",
        "@grpc/grpc-js",
    ];

    let mut front_score: i32 = 0;
    let mut back_score: i32 = 0;
    for dep in dependencies {
        if frontend.contains(&dep.as_str()) {
            front_score += 3;
        }
        if backend.contains(&dep.as_str()) {
            back_score += 3;
        }
    }

    let name_lower = name.to_lowercase();
    for (token, is_front) in [
        ("mfe", true),
        ("frontend", true),
        ("front-end", true),
        ("web", true),
        ("ui", true),
        ("client", true),
        ("service", false),
        ("microservice", false),
        ("api", false),
        ("backend", false),
        ("back-end", false),
        ("server", false),
        ("worker", false),
        ("gateway", false),
    ] {
        if name_lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|part| part == token)
        {
            if is_front {
                front_score += 3;
            } else {
                back_score += 3;
            }
        }
    }

    let script_text = scripts
        .values()
        .map(|script| script.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    if script_text.contains("@vue/cli-service")
        || script_text.contains("react-scripts")
        || script_text.contains("vite")
        || script_text.contains("next dev")
        || script_text.contains("ng serve")
    {
        front_score += 2;
    }
    if script_text.contains("nest start") || script_text.contains("ts-node") {
        back_score += 1;
    }

    if front_score > back_score {
        ProjectKind::Mfe
    } else if back_score > front_score {
        ProjectKind::Service
    } else {
        ProjectKind::App
    }
}

fn merge_patterns(defaults: &[&str], extras: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    defaults
        .iter()
        .map(|pattern| pattern.to_lowercase())
        .chain(extras.iter().map(|pattern| pattern.to_lowercase()))
        .filter(|pattern| !pattern.is_empty() && seen.insert(pattern.clone()))
        .collect()
}

fn relative_id(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let id = relative.to_string_lossy().replace('\\', "/");
    if id.is_empty() { ".".to_string() } else { id }
}

fn dedupe_services(specs: &mut Vec<ServiceSpec>, warnings: &mut Vec<String>) {
    let mut seen = HashSet::new();
    specs.retain(|spec| {
        if seen.insert(spec.id.clone()) {
            true
        } else {
            warnings.push(format!(
                "Duplicate service path {} ({}); skipped",
                spec.id, spec.name
            ));
            false
        }
    });
}

static PORT_LOCALHOST_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)(?:https?://)?(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):(\d{2,5})")
        .expect("valid port regex")
});

static PORT_WORD_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\bport(?:\s*[:=]\s*|\s+)(\d{2,5})\b").expect("valid port regex")
});

/// Extracts a likely listening port from a log line.
pub fn extract_port(line: &str) -> Option<u16> {
    for regex in [&*PORT_LOCALHOST_RE, &*PORT_WORD_RE] {
        if let Some(captures) = regex.captures(line)
            && let Some(port) = captures.get(1)
            && let Ok(port) = port.as_str().parse::<u16>()
            && port > 0
        {
            return Some(port);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn script_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    fn write_project(root: &Path, relative: &str, name: &str, scripts_json: &str, deps: &str) {
        let dir = root.join(relative);
        fs::create_dir_all(&dir).unwrap();
        let contents =
            format!(r#"{{"name":"{name}","scripts":{scripts_json},"dependencies":{{{deps}}}}}"#);
        fs::write(dir.join("package.json"), contents).unwrap();
    }

    #[test]
    fn picks_dev_before_serve_and_start() {
        let scripts = script_map(&[
            ("start", "node ."),
            ("serve", "vue-cli-service serve"),
            ("dev", "vite"),
        ]);
        assert_eq!(pick_start_script(&scripts).as_deref(), Some("dev"));

        let scripts = script_map(&[("start", "node ."), ("serve", "vue-cli-service serve")]);
        assert_eq!(pick_start_script(&scripts).as_deref(), Some("start"));
        assert_eq!(CommandSpec::npm_script("start").display(), "npm start");

        let scripts = script_map(&[("build", "tsc"), ("serve", "vue-cli-service serve")]);
        assert_eq!(pick_start_script(&scripts).as_deref(), Some("serve"));

        let scripts = script_map(&[("build", "tsc"), ("lint", "eslint .")]);
        assert_eq!(pick_start_script(&scripts), None);
    }

    #[test]
    fn picks_prefixed_dev_script() {
        let scripts = script_map(&[("dev:web", "vite"), ("start:api", "node server.js")]);
        assert_eq!(pick_start_script(&scripts).as_deref(), Some("dev:web"));
    }

    #[test]
    fn infers_project_kinds() {
        let scripts = script_map(&[("dev", "vite")]);
        assert_eq!(
            infer_kind("shell", &["vue".to_string(), "vite".to_string()], &scripts),
            ProjectKind::Mfe
        );
        assert_eq!(
            infer_kind("orders", &["express".to_string()], &scripts),
            ProjectKind::Service
        );
        assert_eq!(
            infer_kind("payments-api", &[], &scripts),
            ProjectKind::Service
        );
        let no_scripts = BTreeMap::new();
        assert_eq!(infer_kind("scripts", &[], &no_scripts), ProjectKind::App);
    }

    #[test]
    fn discovers_nested_projects_and_skips_noise() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_project(
            root,
            "apps/shell",
            "shell",
            r#"{"dev":"vite"}"#,
            r#""vue":"^3.0.0""#,
        );
        write_project(
            root,
            "services/orders",
            "orders",
            r#"{"start":"node server.js"}"#,
            r#""express":"^4.0.0""#,
        );
        write_project(root, "node_modules/noise", "noise", r#"{"dev":"vite"}"#, "");
        write_project(root, "tools/no-start", "no-start", r#"{"build":"tsc"}"#, "");
        fs::write(
            root.join("services-manager.toml"),
            "[workspace]\nstop_timeout_secs = 9\n",
        )
        .unwrap();

        let workspace = Workspace::load(root, None, true).unwrap();
        assert_eq!(workspace.specs.len(), 2);
        assert_eq!(workspace.specs[0].stop_timeout, Duration::from_secs(9));
        let names: Vec<_> = workspace
            .specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect();
        assert_eq!(names, vec!["shell", "orders"]);
        assert_eq!(workspace.specs[0].kind, ProjectKind::Mfe);
        assert_eq!(workspace.specs[1].kind, ProjectKind::Service);
        assert_eq!(workspace.specs[0].command.display(), "npm run dev");
        assert!(
            workspace
                .warnings
                .iter()
                .any(|warning| warning.contains("no dev, start, or serve"))
        );
    }

    #[test]
    fn descends_through_package_dirs_without_a_start_script() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_project(root, "apps", "tooling", r#"{"build":"tsc"}"#, "");
        write_project(
            root,
            "apps/shell",
            "shell",
            r#"{"dev":"vite"}"#,
            r#""vue":"^3.0.0""#,
        );

        let workspace = Workspace::load(root, None, true).unwrap();
        assert_eq!(workspace.specs.len(), 1);
        assert_eq!(workspace.specs[0].name, "shell");
    }

    #[test]
    fn example_config_parses() {
        let config: FileConfig =
            toml::from_str(include_str!("../services-manager.toml.example")).unwrap();
        assert_eq!(config.services.len(), 4);
    }

    #[test]
    fn config_defines_services_and_overrides_commands() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_project(
            root,
            "apps/shell",
            "shell",
            r#"{"dev":"vite"}"#,
            r#""vue":"^3.0.0""#,
        );
        fs::create_dir_all(root.join("services/orders")).unwrap();
        fs::write(
            root.join("services-manager.toml"),
            r#"
[workspace]
ready_timeout_secs = 3
metrics = true

[[services]]
name = "Shell"
path = "apps/shell"
type = "mfe"

[[services]]
name = "Orders API"
path = "services/orders"
kind = "service"
start = ["node", "server.js"]
port = 4000
env = { NODE_ENV = "development" }
ready_patterns = ["orders api ready"]
"#,
        )
        .unwrap();

        let workspace = Workspace::load(root, None, false).unwrap();
        assert_eq!(workspace.specs.len(), 2);
        assert_eq!(workspace.options.ready_timeout, Duration::from_secs(3));
        assert!(!workspace.options.metrics_enabled);
        let orders = workspace
            .specs
            .iter()
            .find(|spec| spec.name == "Orders API")
            .unwrap();
        assert_eq!(orders.command.display(), "node server.js");
        assert_eq!(orders.port, Some(4000));
        assert_eq!(orders.env.get("NODE_ENV").unwrap(), "development");
        assert!(orders.line_is_ready("orders api ready on 4000"));
        assert_eq!(orders.kind, ProjectKind::Service);
    }

    #[test]
    fn falls_back_to_root_package() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("package.json"),
            r#"{"name":"single","scripts":{"dev":"vite"},"dependencies":{"react":"^19"}}"#,
        )
        .unwrap();
        let workspace = Workspace::load(temp.path(), None, true).unwrap();
        assert_eq!(workspace.specs.len(), 1);
        assert_eq!(workspace.specs[0].id, ".");
        assert_eq!(workspace.specs[0].name, "single");
    }

    #[test]
    fn extracts_ports_from_common_readiness_logs() {
        assert_eq!(
            extract_port("  ➜  Local:   http://localhost:5173/"),
            Some(5173)
        );
        assert_eq!(extract_port("Server listening on port 3000"), Some(3000));
        assert_eq!(extract_port("http://127.0.0.1:8080/api"), Some(8080));
        assert_eq!(extract_port("no port here"), None);
    }

    #[test]
    fn avoids_flagging_zero_error_lines() {
        let spec = ServiceSpec {
            id: "test".into(),
            name: "test".into(),
            path: PathBuf::from("."),
            kind: ProjectKind::App,
            command: CommandSpec::npm_script("dev"),
            env: BTreeMap::new(),
            ready_patterns: merge_patterns(DEFAULT_READY_PATTERNS, &[]),
            error_patterns: merge_patterns(DEFAULT_ERROR_PATTERNS, &[]),
            port: None,
            stop_timeout: Duration::from_secs(3),
        };
        assert!(!spec.line_is_error("compiled with 0 errors"));
        assert!(spec.line_is_error("error: cannot find module 'vue'"));
        assert!(spec.line_is_error("✘ [ERROR] Could not resolve \"./missing\""));
        assert!(spec.line_is_ready("server running on port 3000"));
    }
}
