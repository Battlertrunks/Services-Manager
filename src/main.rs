mod app;
mod bundles;
mod metrics;
mod process;
mod service;
mod text;
mod ui;
mod workspace;

#[cfg(test)]
mod bundle_tests;

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use ratatui::DefaultTerminal;
use ratatui::crossterm;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};

use crate::app::App;
use crate::metrics::MetricsMonitor;
use crate::workspace::Workspace;

const HELP: &str = "\
services-manager — a terminal dashboard for local MFEs and micro-services

USAGE:
    services-manager [ROOT] [OPTIONS]

ARGS:
    ROOT    Directory containing Node projects [default: .]

OPTIONS:
    -c, --config <PATH>    Use a specific services-manager.toml
        --no-metrics       Disable CPU and memory sampling
    -h, --help             Print this help
    -V, --version          Print the version

By default, services are discovered in child directories (up to two levels deep).
Each project needs a package.json with a dev, start, or serve script.

Keys:
    ↑↓←→ / hjkl    move          enter/space    start or stop
    o              logs          b              bundles
    a / x          start all / stop all
    r              rescan        ?              help
    q / ctrl-c     quit (asks first when services are running)

Config:
    Optional services-manager.toml in ROOT. See the README for a full example.
";

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("services-manager: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let action = match parse_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(message) => {
            eprintln!("services-manager: {message}\n");
            eprint!("{HELP}");
            std::process::exit(2);
        }
    };

    let cli = match action {
        CliAction::Help => {
            print!("{HELP}");
            return Ok(());
        }
        CliAction::Version => {
            println!("services-manager {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliAction::Run(cli) => cli,
    };

    let explicit_config = cli.config.is_some();
    let workspace = Workspace::load(&cli.root, cli.config.as_deref(), cli.metrics)
        .with_context(|| format!("cannot load workspace {}", cli.root.display()))?;
    let config_override = if explicit_config {
        workspace.config_path.clone()
    } else {
        None
    };
    let mut app = App::new(workspace, config_override, cli.metrics);

    let mut terminal = init_terminal()?;
    let mut metrics = if app.metrics_enabled {
        Some(MetricsMonitor::start(
            app.services.clone(),
            app.options.metrics_interval,
        ))
    } else {
        None
    };

    let run_result = run_tui(&mut terminal, &mut app, metrics.as_ref()).await;

    let disable_mouse = execute!(terminal.backend_mut(), DisableMouseCapture);
    drop(terminal);
    let restore = ratatui::try_restore();
    if let Some(monitor) = metrics.as_mut() {
        monitor.stop();
    }

    let running = app.running_count();
    if running > 0 {
        eprintln!(
            "Stopping {running} running service{}…",
            if running == 1 { "" } else { "s" }
        );
    }
    let stopped = process::shutdown_all(&app.services).await;
    if stopped > 0 {
        eprintln!(
            "Stopped {stopped} service{}.",
            if stopped == 1 { "" } else { "s" }
        );
    }

    disable_mouse.context("cannot disable mouse capture")?;
    restore.context("cannot restore terminal")?;
    run_result
}

async fn run_tui(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    metrics: Option<&MetricsMonitor>,
) -> Result<()> {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    spawn_input_thread(event_tx);
    let mut signals = TerminationSignals::new()?;

    let mut ticker = interval(Duration::from_millis(150));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        terminal
            .draw(|frame| ui::render(frame, app))
            .context("cannot draw UI")?;

        tokio::select! {
            _ = ticker.tick() => app.tick(),
            _ = signals.recv() => {
                // External SIGTERM/SIGHUP/SIGINT: skip the confirmation dialog, but still run
                // the normal terminal and service cleanup path.
                app.should_quit = true;
            }
            maybe_event = event_rx.recv() => {
                match maybe_event {
                    Some(Event::Key(key))
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        app.handle_key(key);
                    }
                    Some(Event::Mouse(mouse)) => app.handle_mouse(mouse),
                    Some(Event::Resize(_, _)) => {}
                    Some(_) => {}
                    None => {
                        // The input thread died; there is no way to interact any more.
                        app.should_quit = true;
                    }
                }
            }
        }

        if app.take_services_dirty()
            && let Some(monitor) = metrics
        {
            monitor.update_services(app.services.clone());
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn spawn_input_thread(tx: mpsc::UnboundedSender<Event>) {
    std::thread::Builder::new()
        .name("services-input".to_string())
        .spawn(move || {
            loop {
                match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Release => {}
                    Ok(Event::Mouse(mouse))
                        if matches!(
                            mouse.kind,
                            MouseEventKind::Moved | MouseEventKind::Drag(_)
                        ) => {}
                    Ok(event) => {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();
}

fn init_terminal() -> Result<DefaultTerminal> {
    let mut terminal = ratatui::try_init().context("cannot initialize terminal")?;
    if let Err(err) = execute!(terminal.backend_mut(), EnableMouseCapture) {
        let _ = ratatui::try_restore();
        return Err(err).context("cannot enable mouse capture");
    }

    // Ratatui's init hook restores raw mode and the alternate screen. Wrap it so mouse capture
    // is disabled and child process groups are terminated before that restoration happens.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = process::kill_all_process_groups_now();
        let _ = execute!(stdout(), DisableMouseCapture);
        previous_hook(info);
    }));

    Ok(terminal)
}

#[cfg(unix)]
struct TerminationSignals {
    sigterm: tokio::signal::unix::Signal,
    sighup: tokio::signal::unix::Signal,
    sigint: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl TerminationSignals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            sigterm: signal(SignalKind::terminate())?,
            sighup: signal(SignalKind::hangup())?,
            sigint: signal(SignalKind::interrupt())?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.sigterm.recv() => {}
            _ = self.sighup.recv() => {}
            _ = self.sigint.recv() => {}
        }
    }
}

#[cfg(not(unix))]
struct TerminationSignals;

#[cfg(not(unix))]
impl TerminationSignals {
    fn new() -> std::io::Result<Self> {
        Ok(Self)
    }

    async fn recv(&mut self) {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Debug)]
struct Cli {
    root: PathBuf,
    config: Option<PathBuf>,
    metrics: bool,
}

enum CliAction {
    Run(Cli),
    Help,
    Version,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> std::result::Result<CliAction, String> {
    let mut root: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut metrics = true;
    let mut positional_only = false;

    while let Some(arg) = args.next() {
        if !positional_only {
            match arg.as_str() {
                "-h" | "--help" => return Ok(CliAction::Help),
                "-V" | "--version" => return Ok(CliAction::Version),
                "--no-metrics" => {
                    metrics = false;
                    continue;
                }
                "-c" | "--config" => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("{arg} requires a path"))?;
                    config = Some(PathBuf::from(value));
                    continue;
                }
                "--" => {
                    positional_only = true;
                    continue;
                }
                value if value.starts_with("--config=") => {
                    config = Some(PathBuf::from(&value["--config=".len()..]));
                    continue;
                }
                value if value.starts_with('-') && value != "-" => {
                    return Err(format!("unknown option: {value}"));
                }
                _ => {}
            }
        }

        if root.is_some() {
            return Err(format!("unexpected argument: {arg}"));
        }
        root = Some(PathBuf::from(arg));
    }

    if config
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        return Err("--config path cannot be empty".to_string());
    }

    Ok(CliAction::Run(Cli {
        root: root.unwrap_or_else(|| PathBuf::from(".")),
        config,
        metrics,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root_config_and_metrics() {
        let action = parse_args(
            ["my-workspace", "--config", "custom.toml", "--no-metrics"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        let CliAction::Run(cli) = action else {
            panic!("expected run");
        };
        assert_eq!(cli.root, PathBuf::from("my-workspace"));
        assert_eq!(cli.config, Some(PathBuf::from("custom.toml")));
        assert!(!cli.metrics);
    }

    #[test]
    fn rejects_unknown_flags_and_extra_roots() {
        assert!(parse_args(["--wat"].into_iter().map(str::to_string)).is_err());
        assert!(parse_args(["a", "b"].into_iter().map(str::to_string)).is_err());
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert!(matches!(
            parse_args(["--help"].into_iter().map(str::to_string)).unwrap(),
            CliAction::Help
        ));
        assert!(matches!(
            parse_args(["-V"].into_iter().map(str::to_string)).unwrap(),
            CliAction::Version
        ));
    }
}
