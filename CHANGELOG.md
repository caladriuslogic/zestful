# Changelog

All notable changes to the Zestful CLI will be documented in this file.

## [Unreleased]

### Added
- Event protocol v1: `POST /events` on the Rust daemon (`127.0.0.1:21548`) accepts structured event envelopes from emitters. `zestful hook` now emits events for each incoming agent-hook payload in parallel with the legacy `/notify` call. No downstream consumers in v1 — ingestion-only, to begin populating the event corpus. See `docs/superpowers/specs/2026-04-21-event-protocol-design.md`.
- Daemon now forwards accepted events to the Fly backend (`POST /v1/events`) using the Supabase JWT the Mac app writes to `~/.config/zestful/supabase.jwt`. Fire-and-forget via `tokio::spawn`; errors logged and swallowed so hook latency and local log behavior are unchanged.

## [3.1.0] - 2026-04-03

### Added
- `zestful inspect` — inspect running terminals, multiplexers, IDEs, and browsers
  - Subcommands: `terminals`, `tmux`, `shelldon`, `zellij`, `ides`, `browsers`, `where`, `all`
  - `--pretty` flag for human-readable output, JSON by default
  - `zestful inspect where` prints the `workspace://` URI for the current terminal location
- `zestful focus` — focus a terminal tab directly from the CLI
  - Accepts a `workspace://` URI positional arg or `--app`/`--window-id`/`--tab-id` flags
  - Same focus logic as the daemon, no HTTP round-trip
  - Handles shelldon and tmux multiplexer segments embedded in URIs
- tmux focus — clicking a notification from inside tmux switches to the correct tmux window and pane
  - Parses `tmux:<session>/window:<idx>/pane:<idx>` segments from `workspace://` URIs
  - Runs `tmux select-window` + `tmux select-pane` independently of terminal tab focus
  - Works in both daemon and `zestful focus` CLI
- Built-in workspace inspector — terminal/multiplexer/IDE/browser detection is now part of the zestful binary
  - Detects: iTerm2, kitty, WezTerm, Terminal.app, Alacritty, Ghostty, GNOME Terminal, Command Prompt, PowerShell
  - Multiplexers: tmux, zellij, shelldon
  - IDEs: Xcode
  - Browsers: Google Chrome
  - Focus handlers merged with detection — detect and focus code for each terminal lives in one module

### Fixed
- iTerm2 window focus now raises the correct window by its AppleScript ID instead of activating a random window
- iTerm2 split/pane detection — each pane is now a separate entry identified by TTY, fixes wrong tab numbers when tabs have splits
- iTerm2 pane-level focus — focuses the exact pane within a tab, not just the tab
- Kitty detection and focus rewritten — uses kitty's internal window IDs for reliable focus down to the exact split/pane

### Changed
- iTerm2 focus switched from iterm2-client API to AppleScript for both window and tab — detection and focus now use the same technique
- Kitty focus uses `kitty @ focus-window --match id:{id}` which handles OS window, tab, and split switching in one command
- Kitty `locate()` uses `KITTY_WINDOW_ID` env var for instant detection without TTY matching
- Kitty socket discovery improved — checks `KITTY_LISTEN_ON`, PID-suffixed paths, and `/tmp` scan
- Terminal detection and focus code merged into `src/workspace/` module tree
- Focus dispatch moved from `src/focus/` to `src/workspace/terminals/`
- URI parsing moved to `src/workspace/uri.rs`

### Removed
- `workspace-inspector` external crate dependency — all detection code is now built-in
- `iterm2-client` dependency — iTerm2 focus now uses AppleScript directly
- `src/focus/` directory — replaced by `src/workspace/terminals/` (merged detect+focus)

## [3.0.0] - 2026-03-19

Complete rewrite from bash/Node.js to Rust. A single static binary replaces the
bash CLI script, Node.js focus daemon, and Python iTerm2 dependency.

### Added
- `zestful notify` — send notifications to the Zestful macOS app via sync HTTP (ureq)
- `zestful watch` — wrap a command and notify on completion with exit-code-based severity
- `zestful ssh` — sync config to remote host and `exec ssh -R` for port forwarding
- `zestful daemon` — axum server on localhost:21548 with `/health` and `/focus` endpoints
- Native iTerm2 tab switching via `iterm2-client` crate (no Python dependency)
- Focus handlers for kitty, WezTerm, Terminal.app, and generic apps (osascript)
- Auto-start daemon from CLI commands via PID file check
- 68 unit tests

### Security
- Input validation on all focus IDs (app, window_id, tab_id) — rejects shell metacharacters
- AppleScript string escaping prevents command injection via osascript
- Token authentication (`X-Zestful-Token`) required on daemon `/focus` endpoint
- 16KB request body size limit on daemon prevents memory exhaustion
- `--severity` validated against `info`/`warning`/`urgent` at parse time
- PID file symlink check prevents arbitrary file overwrite
- Kitty socket validated as real Unix socket, not a symlink
- Config directory created with mode 0700
- Error messages redacted to prevent token leakage
- `kill(pid, 0)` guarded against pid <= 0

### Removed
- Node.js dependency (zestfuld.js daemon)
- Python dependency (iTerm2 tab switching)
- Bash CLI script

### Breaking Changes
- Requires Rust toolchain to build from source (or use pre-built binary)
- Binary replaces both `zestful` (bash) and `zestfuld.js` (Node.js daemon)
