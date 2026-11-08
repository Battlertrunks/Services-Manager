# AGENTS.md

This repository follows the [AGENTS.md](https://agents.md) convention. This file is the operating
guide for AI coding agents working here. Human instructions in the current conversation always
take precedence. If this file conflicts with a direct request, ask the developer instead of
guessing.

## 1. Project overview

`services-manager` is a Rust terminal UI (TUI) for running local Node MFEs and micro-services side
by side. Point it at a workspace directory; it discovers projects with runnable `package.json`
scripts, starts and stops them in isolated process groups, streams logs, samples CPU and memory,
and persists service bundles in SQLite.

- Binary crate only (no library target), Rust 2024 edition, minimum Rust 1.95.
- UI: `ratatui` + `crossterm`. Runtime: `tokio`. Storage: `rusqlite` (bundled SQLite). Metrics:
  `sysinfo`.
- `README.md` is the user-facing source of truth for behavior, discovery rules, status lights,
  logs, bundles, metrics, keyboard shortcuts, and configuration. Read it before changing any of
  those areas.
- `services-manager.toml.example` is the canonical example config. A test parses it directly.

## 2. Scope discipline: do only what was asked

You are an implementation agent. The developer or orchestrator defines the task; complete exactly
that task and nothing more.

- Do not start side quests, opportunistic refactors, cleanups, renames, formatting passes,
  dependency upgrades, or "while I'm here" changes.
- Do not modify adjacent files unless the requested change genuinely requires it and you have
  approval. If you think another file must change, explain why and ask first.
- Keep diffs minimal and focused. Follow existing patterns instead of rewriting working code.
- If you notice an unrelated bug, security issue, dead code, or improvement, report it. Do not fix
  it without approval.
- If requirements are ambiguous, incomplete, or contradictory, stop and ask the developer. Do not
  assume. A short question is cheaper than an unwanted change.
- If a requested change cannot be completed without a new dependency, a schema change, a new
  secret/key/environment variable, or touching unrelated modules, stop and get approval before
  proceeding.

## 3. Repository layout

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI parsing, terminal lifecycle, panic hook, event loop, signal-driven shutdown |
| `src/app.rs` | UI state, user actions, key/mouse handling, overlays, rescan, bundle activation |
| `src/workspace.rs` | Project discovery, `package.json`/TOML parsing, service specs, readiness/error patterns, port extraction |
| `src/service.rs` | Per-service runtime state, statuses, log ring buffer, log metadata, metrics snapshot |
| `src/process.rs` | Spawning services in process groups, log streaming, readiness fallback, stop/SIGKILL escalation, shutdown |
| `src/metrics.rs` | Background CPU/memory sampling for each service's process tree |
| `src/bundles.rs` | SQLite-backed bundle storage and CRUD |
| `src/text.rs` | ANSI/control-sequence stripping and UTF-8-safe truncation |
| `src/bundle_tests.rs` | Test-only behavioral tests for bundles, UI flows, and process cleanup |
| `src/ui/mod.rs` | Root render dispatch, layout helpers, overlays, shared widgets |
| `src/ui/dashboard.rs` | Service card grid |
| `src/ui/bundles.rs` | Bundle manager, editor, and delete confirmation |
| `src/ui/logs.rs` | Live logs modal |
| `src/ui/help.rs` | Help and discovery warnings |
| `Cargo.toml` / `Cargo.lock` | Dependencies and locked versions; the lockfile is tracked for this binary crate |
| `README.md` | User-facing documentation and behavior reference |
| `services-manager.toml.example` | Example workspace config, also exercised by tests |
| `.services-manager/` | Local runtime data (bundle SQLite DB); gitignored and not part of the source |
| `target/` | Cargo build output; never commit |

Tests are colocated in `#[cfg(test)]` modules. There is no separate `tests/` directory and no CI
workflow; quality gates are run locally.

## 4. Environment and build setup

Requirements:

- Rust 1.95 or newer. Check with `rustc --version` and `cargo --version`.
- A C toolchain for the bundled SQLite build (`rusqlite` with the `bundled` feature). On macOS,
  install Xcode Command Line Tools if compilation fails; on Linux, install a C compiler such as
  `clang` or `gcc`.
- Node.js and npm on `PATH` only when manually exercising real service discovery and process
  management. The Rust test suite itself does not require Node; process tests use `sh` fixtures.
- macOS or Linux is the primary target. Windows support is best-effort and uses `taskkill`.

Common commands:

```sh
cargo check --all-targets          # fast type-check
cargo build                        # debug binary at target/debug/services-manager
cargo build --release              # release binary at target/release/services-manager
cargo run -- --help                # CLI smoke check
cargo run -- --version
cargo run -- <workspace-root>      # requires an interactive terminal
cargo test                         # full test suite
cargo test workspace::tests        # focused module tests
cargo test bundle_tests::          # focused behavioral tests
```

No environment file, external database, or network service is required for normal development.
The application uses in-memory logs and a workspace-local SQLite file for bundles.

## 5. Sandboxing and safe test workspaces

Never point the application at this repository or at a real user workspace while developing.

- Use a disposable workspace directory created with `mktemp -d` (prefer `$TMPDIR`) or
  `tempfile::tempdir()` in tests.
- Put fixture `package.json` files inside the temporary workspace. Use tiny scripts that exit
  quickly or bounded `sleep` commands. Avoid real `npm install`, network access, and dependency
  downloads.
- Pass `--no-metrics` for manual CLI smoke runs when metrics are irrelevant.
- Do not launch the TUI from a non-interactive shell; it needs a real terminal. For automated
  verification, prefer unit tests, `ratatui::backend::TestBackend` UI tests, and `--help`/
  `--version` checks.
- Process tests must use bounded fixtures (`sh -c` with a short `sleep`), clean up child processes
  on both success and failure, and avoid fixed ports. Follow the existing `ServiceCleanup` guard
  pattern in `src/bundle_tests.rs`.
- Tests that write files must use `tempfile::tempdir()` and must not touch the repository's
  `.services-manager/` directory, the user's home directory, or a real workspace.
- If a task needs a real Node/npm integration test and the environment cannot support it, say so
  and ask before installing packages or changing the environment.
- For risky or large changes, consider a separate git worktree or temporary copy of the repo so
  the developer's working tree is untouched. Never run destructive git commands against the
  developer's checkout.

## 6. Required quality gates

Before declaring a task complete, all of these must pass from the repository root:

1. `cargo fmt --check`
2. `cargo clippy --all-targets -- -D warnings`
3. `cargo test`
4. `cargo build --release` when the change affects release behavior, packaging, or the developer
   asks for a release build.

`cargo check --all-targets` is a useful fast pre-check, but it does not replace the gates above.

Rules:

- Fix all failures and warnings. Do not leave the tree in a state where a gate was skipped.
- Do not silence warnings with broad `#[allow(...)]` attributes. If an allow is genuinely required,
  keep it narrow and explain why in the final summary.
- Do not delete, ignore, or weaken tests to get a green result.
- If a gate fails for a reason that predates your change, verify the baseline (for example in a
  clean worktree) and report it clearly with the exact command and output. Do not hide it.
- If you cannot run a gate, state exactly which gate was not run and why. Never claim completion
  without disclosure.
- Add or update tests for every behavior change. Bug fixes should include a regression test.
- Re-run the gates after the final edit, not just before an intermediate commit.

## 7. Testing expectations

- Put unit tests in a local `#[cfg(test)] mod tests` near the code they exercise.
- Use `#[tokio::test]` for async runtime behavior and `tempfile::tempdir()` for filesystem tests.
- Use `ratatui::backend::TestBackend` for UI tests. Render at several terminal sizes, including
  the small-size fallback path; `src/bundle_tests.rs` has `render_at_common_sizes` as a reference.
- Process and signal tests are Unix-gated. Keep them bounded, deterministic, and leak-free.
- Tests must not depend on the developer's machine state, user home, fixed ports, network access,
  installed npm packages, or execution order.
- For discovery changes, cover both auto-discovery and explicit TOML configuration.
- For config changes, update `services-manager.toml.example` and keep
  `workspace::tests::example_config_parses` passing. Add tests for validation failures and
  defaults.
- For bundle changes, cover persistence, update/delete, missing members, and the UI flow where
  applicable.
- Preserve stable service IDs: persisted bundles refer to them. Changing the ID scheme can break
  existing bundle data and needs explicit approval plus a migration plan.
- Use deadline-based wait helpers instead of arbitrary sleeps when waiting for asynchronous
  process state.

## 8. Dependency policy

- Do not add, remove, or upgrade dependencies without explicit developer approval.
- Prefer implementing behavior from scratch with the standard library and existing dependencies.
  If a dependency is clearly the better engineering choice, make the case before changing
  `Cargo.toml`.
- Never run `cargo add` or an unrestricted `cargo update` as part of a feature task without
  approval.
- When requesting a dependency, present all of the following to the developer first:
  - Why the standard library and current dependencies are insufficient.
  - Alternatives considered, including a scratch implementation.
  - Exact crate, version, features, and expected `Cargo.lock` impact.
  - Maintenance status, release recency, license, and transitive dependency footprint.
  - Known vulnerability history and current advisories (RustSec advisory database, GitHub Security
    Advisories, and CVE/NVD where relevant).
- Do not add build scripts, proc macros, FFI crates, vendored source, or downloaded binaries/assets
  without approval.
- Keep `Cargo.lock` changes limited to approved dependency changes.

## 9. Security, secrets, keys, and environment variables

Security is part of every task. Treat workspace configuration and `package.json` files as
untrusted input: `services-manager` executes commands found there.

Non-negotiables:

- Never write real API keys, tokens, passwords, private keys, connection strings, or other secrets
  into source, tests, fixtures, comments, logs, commit messages, or the final response.
- Do not print secret values in command output, debug output, or test failures. If you must show a
  value, redact it.
- Do not leave important keys, properties, or environment variables "open": unredacted, unset,
  exposed in tracked files, or undocumented. If a change adds, changes, or requires any of the
  following, explicitly tell the developer in the final summary:
  - environment variables
  - config keys/properties under `[workspace]` or `[[services]]`
  - CLI flags or defaults
  - credentials, tokens, keys, or permission scopes
  - database fields, tables, or file paths that carry sensitive data
  - secrets that must be supplied before the application can run
- For each new or changed item, report: name, purpose, required/optional, expected format, where
  it is set, safe default, and whether the developer must act. Use placeholders such as
  `<YOUR_API_KEY>`; never a real value.
- When a task needs a new config key, update `services-manager.toml.example` and the relevant tests
  only if that is in scope; otherwise flag the need and ask.
- Do not commit a real `services-manager.toml` that contains secrets. The example file is safe;
  an actual workspace config may not be. Ensure secret-bearing local files are gitignored in the
  workspace where they live.
- Preserve the existing command execution model: commands are split into arguments and executed
  directly, not through a shell. Do not add shell interpretation, command interpolation, or
  string concatenation that could introduce injection. Shell syntax should remain an explicit
  `["sh", "-c", ...]` choice by the user.
- Keep SQL parameterized (`params!`/bound parameters); never interpolate user input into SQL.
  Changing the bundle schema requires a migration plan and approval.
- Preserve ANSI and control-character stripping before text reaches the terminal; it protects
  against terminal escape injection from child output.
- Treat `unsafe` code and process-group signaling as security-sensitive. Keep `SAFETY` comments,
  verify platform `cfg` branches, and never signal unintended PIDs or process groups.
- Do not add telemetry, analytics, remote logging, or network calls without approval.
- Do not persist service logs or transmit them off-machine. Logs can contain secrets from the
  managed applications.
- If you discover a security concern, report it to the developer immediately. Do not quietly work
  around it or fix it outside scope.

## 10. Code comments

Add comments where they help a developer understand why a decision was made, not what an obvious
line does.

- Use `//!` for module-level context, `///` for public or non-obvious items, and `//` for local
  rationale.
- Required comment sites: `unsafe` blocks, platform-specific code, process-group and signal
  lifecycle logic, readiness/error heuristics, non-obvious state transitions, workarounds,
  security decisions, and schema/data migrations.
- Explain invariants and tradeoffs. For example: why a fallback exists, why a signal is escalated,
  why a value is intentionally not persisted, or why a test avoids a real network.
- Keep comments accurate and update them when behavior changes. Do not leave stale comments.
- Do not add noise that restates the code. Match the existing sentence-case, explanatory style.

## 11. Code style and conventions

- Use default `rustfmt`; do not add formatting exceptions.
- Clippy warnings are errors for all targets (`-D warnings`).
- Follow the existing module boundaries and naming. Reuse existing helpers rather than duplicating
  logic.
- Error handling uses `anyhow::Result`, `.context(...)`/`with_context(...)`, and `bail!` for
  validation. Avoid `unwrap`, `expect`, and `panic!` in production paths. Existing poisoned-lock
  recovery (`unwrap_or_else(|poisoned| poisoned.into_inner())`) is intentional; preserve it.
- Keep the UI loop responsive. Do not block the rendering thread with process, filesystem, or
  network work; use tasks or background threads as the existing code does.
- Never use `println!` or `dbg!` in TUI code; it corrupts the terminal. Use `App` messages,
  service logs, or the established error path. `eprintln!` is appropriate only for top-level CLI
  errors before/after the TUI is restored.
- Preserve terminal restoration, mouse capture, panic-hook cleanup, and process-group shutdown
  behavior. Do not simplify away lifecycle code without explicit approval.
- Keep platform behavior behind `#[cfg(unix)]`, `#[cfg(windows)]`, and
  `#[cfg(not(any(unix, windows)))]` as appropriate.
- Keep user-facing behavior and documentation consistent. If a change affects the README or example
  config, report it; update those files only when in scope or approved.

## 12. Git and commits

For long-running features and implementations, make frequent but valuable commits.

- Commit when a coherent, logically complete unit is finished and the relevant checks pass. Do not
  commit just because time has passed, and do not commit unrelated changes together.
- Prefer multiple small, atomic commits over one large commit. Logical splits include: core
  behavior, tests, UI, and documentation.
- Use clear imperative commit subjects, for example `Add bundle member log navigation` or `Fix
  process group cleanup on failed start`. Explain non-obvious motivation in the body.
- Before committing, inspect `git status` and `git diff`; stage only intended files. Never stage
  `target/`, `.services-manager/`, editor files, secrets, or unrelated work.
- Do not rewrite published history, force-push, rebase, or amend commits unless the developer
  explicitly asks.
- Do not push unless asked.
- If a commit would contain incomplete or broken work, keep it uncommitted and report the state
  unless the developer specifically requests a work-in-progress commit.
- Include commit hashes and a short description in the final summary.

## 13. Definition of done

A task is complete only when all of the following are true:

- [ ] The requested behavior is implemented.
- [ ] No unrelated files or behavior were changed without approval.
- [ ] Tests were added or updated for the change, including a regression test for bug fixes.
- [ ] `cargo fmt --check` passes.
- [ ] `cargo clippy --all-targets -- -D warnings` passes.
- [ ] `cargo test` passes.
- [ ] A release build was checked when relevant.
- [ ] No new dependency was added without approval; any proposed dependency has vulnerability
      research attached.
- [ ] No secret, key, config property, or environment variable was left exposed, unset, or
      undocumented. Any addition or change is called out to the developer.
- [ ] Security-sensitive behavior was reviewed and any concerns were reported.
- [ ] User-facing docs or example config were updated if the change affects them and that was in
      scope.
- [ ] The final response lists changed files, commands run and their results, any commit hashes,
      any remaining risks, and any follow-up questions.

## 14. When in doubt

Stop and ask the developer or orchestrator. Ask a specific question, state the options and
tradeoffs, and recommend a default. Do not silently assume, expand scope, or make irreversible
changes to compensate for missing information.
