# services-manager

A terminal dashboard for running local MFEs and micro-services side by side.

Point it at the directory that contains your Node projects. It discovers every project with a
runnable `package.json` script, draws each one as a card, and lets you start, stop, inspect logs,
and watch CPU/memory without leaving the terminal.

```text
▰ services-manager  ~/work/commerce-platform                                    3 running · 4 projects
● 2 ready   ● 1 starting   ● 1 error   ● 0 stopped                                   metrics on
──────────────────────────────────────────────────────────────────────────────────────────────────
 ╭──────────────────────────────────╮ ╭──────────────────────────────────╮ ╭─────────────────╮
 │● ready                        MFE│ │● starting                     MFE│ │● error       SVC│
 │shell-mfe                         │ │checkout-mfe                      │ │payments-api     │
 │apps/shell                        │ │apps/checkout                     │ │services/payments│
 │cpu 1%  ·  mem 142M  ·  :5173     │ │cpu —  ·  mem —                   │ │cpu —  ·  mem —  │
 │ Stop    Logs                     │ │ Stop    Logs                     │ │ Start    Logs   │
 │$ npm run dev                     │ │$ npm start                       │ │✖ cannot find …  │
 ╰──────────────────────────────────╯ ╰──────────────────────────────────╯ ╰─────────────────╯
↑↓←→/hjkl move · enter start/stop · o logs · a start all · x stop all · r rescan · ? help · q quit
▸ shell-mfe  apps/shell  ·  npm run dev  ·  :5173
```

## Features

- Responsive card grid, automatically laid out from the number of projects and terminal size.
- Per-service **Start / Stop** button and a **Logs** button that opens a live, scrollable modal.
- Traffic-light status: green **ready**, yellow **starting**, red **error**, gray **stopped**.
- Log buffering with ANSI stripping, timestamps, follow/pause, horizontal pan, and clear.
- Start all / stop all, workspace rescan, keyboard and mouse control.
- CPU and resident-memory sampling for the entire npm → shell → node process tree.
- Automatic project discovery and type detection (MFE vs service), with an optional TOML config.
- Every service runs in its own process group, so stopping it also stops the node process it
  spawned. Leftover descendants are cleaned up and escalated to SIGKILL when necessary.
- Services are stopped on normal exit (with confirmation if any are running), on external
  SIGTERM/SIGHUP/SIGINT, and by a panic hook.

## Requirements

- Rust 1.95 or newer
- Node.js and npm on `PATH`
- macOS or Linux (Windows is best-effort; process-tree termination uses `taskkill` there)

## Install

```sh
cargo install --path .
```

Or build a release binary:

```sh
cargo build --release
./target/release/services-manager ~/projects
```

## Usage

Run it from the parent directory that contains your apps and services:

```sh
cd ~/projects/commerce-platform
services-manager
```

Or pass a root:

```sh
services-manager ~/projects/commerce-platform
services-manager ~/projects/commerce-platform --no-metrics
services-manager ~/projects/commerce-platform --config ./my-services.toml
```

Useful options:

| Option | Meaning |
| --- | --- |
| `-c, --config <PATH>` | Use a specific config file |
| `--no-metrics` | Disable CPU/memory sampling |
| `-h, --help` | Print help |
| `-V, --version` | Print version |

## How projects are discovered

Without a config file, the app walks the workspace root up to two directory levels deep
(`apps/shell`, `services/orders`, and direct children all work). Hidden directories and common
noise such as `node_modules`, `dist`, `build`, `.next`, `target`, and `coverage` are skipped. A
directory whose `package.json` has a runnable script is treated as a project boundary; a package
without one is searched for nested runnable packages.

For each `package.json`:

1. A start script is selected from `dev`, `start`, `serve`, `develop`, `start:dev`, `dev:server`,
   `start:server`, `serve:dev`, or `watch`, then from names prefixed with `dev:`, `start:`, or
   `serve:`.
2. The command is `npm start` for the `start` script, otherwise `npm run <script>`, executed in the
   project directory.
3. The project is classified as an **MFE**, **SVC**, or **APP** using its dependencies, name, and
   scripts. This only affects the badge; all projects behave the same.
4. If no child project is found, the workspace root itself is used as a single project when it has
   a startable `package.json`.

Projects without a runnable script are skipped and listed as warnings in the help screen (`?`).

## Status lights

| Light | Caption | Meaning |
| --- | --- | --- |
| Green | `ready` | The process is running and either printed a readiness line or stayed alive past the ready timeout without errors. |
| Yellow | `starting` | The process has been launched but has not signalled readiness yet. |
| Red | `error` | A fatal-looking log line was seen or the process exited non-zero. |
| Gray | `stopped` | Not running: exited cleanly without a recorded error, or was stopped by you. |
| Yellow | `stopping` | A stop signal was sent and the process is still shutting down. |

Readiness and error detection is heuristic. The defaults understand common output from Vite,
Webpack, Vue CLI, Next/Nuxt, Angular, Express, Fastify, Nest, and similar tools. If a line contains
a fatal-looking error (for example `error:`, `npm ERR!`, `EADDRINUSE`, `failed to compile`, or a
`✖` marker) the card turns red. A later readiness line makes it green again, which handles dev
servers that recover from a compile error. Add your own phrases in the config if a tool uses
different wording.

If a service never prints a recognized readiness message, it is marked **ready** after
`ready_timeout_secs` (default 8 seconds) as long as it is still running. A clean exit without a
recorded error shows as **stopped**; a clean exit after an error stays red.

## Logs

The `Logs` button (or `o`) opens the selected service's live output. Both stdout and stderr are
captured, ANSI colours are stripped, and the last 2000 lines are retained in memory. Logs survive
stop/restart and are cleared when the service definition is updated by a rescan, or explicitly
with `c` in the modal. Pausing keeps the same line anchored even while new output pushes older
lines out of the ring buffer. Nothing is written to disk.

## Metrics

When enabled, a background thread samples the process tree rooted at each npm process every
2 seconds using [`sysinfo`](https://crates.io/crates/sysinfo). CPU usage is the sum of all
descendants and can exceed 100% on multi-core machines (100% = one core). Memory is the sum of
resident memory. The first sample is usually 0% because CPU deltas need two samples.

Disable sampling with `--no-metrics` or `metrics = false` in the config.

## Keyboard

| Key | Action |
| --- | --- |
| `↑` `↓` `←` `→` / `h` `j` `k` `l` | Move between cards |
| `Tab` / `Shift+Tab` | Cycle selection |
| `Home` / `End` | First / last card |
| `Enter` / `Space` / `s` | Start or stop the selected service |
| `o` | Open live logs |
| `a` | Start all services |
| `x` | Stop all services |
| `r` | Rescan the workspace (only while nothing is running) |
| `?` | Help and discovery warnings |
| `q` / `Ctrl+C` | Quit (asks for confirmation while services are running) |

In the logs modal:

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k` / `PgUp` `PgDn` | Scroll |
| `g` / `G` | Top / tail |
| `f` | Toggle follow mode |
| `←` `→` | Pan horizontally |
| `c` | Clear the buffer |
| `Esc` / `q` | Close |

Mouse works too: click a card's Start/Stop or Logs button, use the wheel to move/scroll. A second
`Ctrl+C` confirms the quit dialog.

## Configuration

Place `services-manager.toml` (or `.services-manager.toml`) in the workspace root, or pass
`--config PATH`. If you define any `[[services]]`, automatic discovery is disabled and the config
is authoritative. Leaving `services` empty keeps auto-discovery while allowing workspace options.

See [`services-manager.toml.example`](services-manager.toml.example) for a complete example.

```toml
[workspace]
scan_depth = 2                  # 1-6, only for auto-discovery
ready_timeout_secs = 8          # fallback "ready" timeout
stop_timeout_secs = 5           # SIGTERM → SIGKILL grace period
log_buffer_lines = 2000
metrics_interval_ms = 2000
metrics = true

[[services]]
name = "Shell MFE"
path = "apps/shell"             # relative to the workspace root
type = "mfe"                    # mfe | service | app; can be inferred
port = 5173                     # optional initial display value
start = "npm run dev"           # string or ["npm", "run", "dev"]
env = { VITE_API_URL = "http://localhost:4000" }
ready_patterns = ["shell ready"]       # added to the defaults
error_patterns = ["fatal boot failure"] # added to the defaults
stop_timeout_secs = 5

[[services]]
name = "Orders API"
path = "services/orders"
kind = "service"                # alias for type
start = ["npm", "run", "start:dev"]
env = { PORT = "4000" }
```

Notes:

- Commands are split like a shell (`shell-words`) but are **executed directly, not through a
  shell**. Shell syntax such as pipes, globs, or `&&` needs an explicit shell, for example
  `start = ["sh", "-c", "npm run dev & npm run watch"]`.
- If `start` is omitted, the command is inferred from the project's `package.json`, just like
  auto-discovery. `path` must contain a `package.json` in that case.
- `ready_patterns` and `error_patterns` extend the built-in lists; matching is case-insensitive.
- Several configured services may point at the same `path` with different names, commands, and
  environments.
- Service definitions are refreshed on `r` (rescan) only when nothing is running. A changed service
  is recreated and starts with an empty log buffer. Changing `log_buffer_lines` recreates the
  services; changing metrics enablement or interval requires restarting the app.
- Only trust workspaces you control: package scripts and configured commands are executed as-is.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Layout of the source:

| Path | Responsibility |
| --- | --- |
| `src/workspace.rs` | Discovery, `package.json` parsing, config, readiness/error patterns |
| `src/service.rs` | Per-service runtime state, statuses, log ring buffer, metrics |
| `src/process.rs` | Spawning npm in process groups, log streaming, stop/shutdown |
| `src/metrics.rs` | Background CPU/memory sampling of process trees |
| `src/app.rs` | UI state, actions, key/mouse handling, rescan |
| `src/ui/` | Card grid, logs modal, help and quit dialogs |
| `src/main.rs` | CLI, terminal lifecycle, async event loop |

## Troubleshooting

- **A card stays yellow** — the service has not printed a recognized readiness line. Wait for the
  ready timeout, or add a phrase to `ready_patterns`.
- **A healthy service turns red** — one of its log lines matched an error pattern. A later
  readiness line clears the error automatically. If it persists, inspect the Logs modal: a real
  compile/runtime error may be scrolling by.
- **A project is missing** — it needs a `dev`/`start`/`serve` script. Check the warnings under `?`,
  or define it explicitly in `services-manager.toml`. Nested projects deeper than two levels
  require a config entry.
- **Ports are occupied after a crash** — stopping and quitting clean up the whole process group,
  including descendants that ignore SIGTERM, and external SIGTERM/SIGHUP/SIGINT trigger the same
  cleanup. If the app was force-killed with SIGKILL, find the leftover node process with
  `lsof -i :<port>` and stop it.
