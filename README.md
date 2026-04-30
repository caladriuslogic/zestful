# Zestful

> Never miss when your AI agent needs you.

Zestful alerts you when Claude Code, Cursor, Aider, or any AI coding agent is waiting for input — floating overlay on Mac, push notifications on iPhone, click to focus the agent's terminal tab.

**Website:** [zestful.dev](https://zestful.dev)

## Install

### CLI (Homebrew)

```bash
brew install caladriuslogic/tap/zestful
```

### CLI (Build from Source)

```bash
git clone https://github.com/caladriuslogic/zestful.git
cd zestful
cargo build --release
cp target/release/zestful /usr/local/bin/
```

### Mac & iOS App

Download from the [App Store](https://zestful.dev) (coming soon).

## Quick Start

1. Install the Zestful Mac app
2. Install the CLI: `brew install caladriuslogic/tap/zestful`
3. Add the hook to your agent (see below)
4. That's it — the overlay flashes when your agent needs you

## Commands

### `zestful notify`

Send a notification to the Zestful overlay.

```bash
zestful notify --agent <name> --message <msg> [options]
```

| Flag | Required | Description |
|------|----------|-------------|
| `--agent` | Yes | Agent name (e.g. `claude-code`, `cursor`) |
| `--message` | Yes | Message to display |
| `--severity` | No | `info`, `warning` (default), or `urgent` |
| `--terminal-uri` | No | Terminal URI for focus (auto-detected) |
| `--no-push` | No | Suppress push notification for this event |

Click-to-focus is automatic — the built-in workspace inspector detects your terminal, window, tab, and multiplexer session. No flags needed.

### `zestful watch`

Wraps any command and notifies when it finishes:

```bash
zestful watch npm run build        # notifies on completion
zestful watch cargo test --release  # notifies on success or failure
zestful watch --agent deploy ./deploy.sh
```

Exit 0 sends `warning` ("done"). Non-zero sends `urgent` ("failed").

### `zestful ssh`

SSH into a remote box with Zestful forwarding. Agents running on the remote machine will notify your local Mac app.

```bash
zestful ssh dev@myserver.com
zestful ssh dev@myserver.com -p 2222 -i ~/.ssh/mykey
```

This copies your auth token, port, and terminal URI to the remote, sets up a reverse port forward, and opens an SSH session. On the remote, `zestful notify` and `zestful watch` work as if you were local — including click-to-focus back to the correct terminal tab on your Mac.

**Manual setup** (for existing scripts or `.ssh/config`):

```bash
# 1. Copy token to remote
scp ~/.config/zestful/local-token dev@myserver.com:~/.config/zestful/local-token

# 2. SSH with reverse port forward
ssh -R 21547:localhost:21547 dev@myserver.com
```

### `zestful daemon`

Starts the focus daemon on `localhost:21548`. The daemon handles terminal tab switching when you click a notification in the Zestful app. It is auto-started by other commands — you rarely need to run this manually.

### `zestful focus`

Focus a terminal tab directly from the command line.

```bash
zestful focus workspace://iterm2/window:26411/tab:3   # by URI
zestful focus --app iTerm2 --tab-id 3                  # by app name + tab
zestful focus --app kitty --window-id 1 --tab-id 7     # with window ID
```

Uses the same focus logic as the daemon but runs directly — no HTTP round-trip. Handles shelldon multiplexer tabs embedded in the URI automatically.

### `zestful inspect`

Inspect running terminals, multiplexers, IDEs, and browsers.

```bash
zestful inspect                     # JSON output of everything
zestful inspect --pretty            # human-readable output
zestful inspect where               # print workspace:// URI for current terminal
zestful inspect terminals --pretty  # just terminal emulators
zestful inspect tmux --pretty       # just tmux sessions
```

Subcommands: `terminals`, `tmux`, `shelldon`, `zellij`, `ides`, `browsers`, `where`, `all` (default).

### `zestful top`

A live, colorful TUI dashboard of agent activity, sourced exclusively from
the daemon's HTTP + SSE API. Master-detail layout: tiles list on the left,
selected tile's metadata, sparkline, recent events, and notifications on
the right.

- **Live** via `GET /stream` (SSE). No polling.
- **Interactive** — `Enter` invokes `POST /focus` to surface the agent's window.
- **Network-only** — also serves as a reference client validating the macOS
  app's contract from any platform (Linux, Windows, remote SSH).

Press `?` inside the TUI for the full keybinding reference.

Requires the Zestful daemon to be running.

### Severity Levels

| Level | Overlay | Menu Bar |
|-------|---------|----------|
| `info` | Returns to "All Clear" (green) | Badge clears |
| `warning` | Pulses amber | Badge shows count |
| `urgent` | Flashes red | Badge shows count |

## Agent Hooks

### Claude Code

Add to `.claude/settings.json` (or copy `hooks/claude-code.json`):

```json
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "zestful notify --agent \"claude-code:$(basename $PWD)\" --message 'Waiting for your input'"
      }]
    }],
    "UserPromptSubmit": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "zestful notify --agent \"claude-code:$(basename $PWD)\" --message 'Working...' --severity info --no-push"
      }]
    }]
  }
}
```

### Aider

One-liner — no config file needed:

```bash
aider --notifications-command 'zestful notify --agent "aider:$(basename $PWD)" --message "$AIDER_NOTIFICATION_TITLE"'
```

### Cursor

Place `.cursor/hooks.json` in your project root (beta):

```json
{
  "hooks": [
    { "event": "stop", "command": "zestful notify --agent \"cursor:$(basename $PWD)\" --message 'Waiting for your input'" },
    { "event": "start", "command": "zestful notify --agent \"cursor:$(basename $PWD)\" --message 'Working...' --severity info" }
  ]
}
```

### GitHub Copilot CLI

Place in `.github/hooks/` (see `hooks/copilot-cli.json`).

### OpenAI Codex CLI

Place `.codex/hooks.json` in your project root (see `hooks/codex-cli.json`).

### Cline

Symlink `hooks/cline-hook.sh` to `~/Documents/Cline/Rules/Hooks/TaskCancel`. Note: only `TaskCancel` is supported (no `TaskComplete` yet).

### Any Script

```bash
zestful watch npm run build
zestful notify --agent "deploy" --message "Deploy needs approval" --severity warning
zestful notify --agent "ci" --message "Build failed!" --severity urgent
```

## Architecture

The `zestful` binary serves two roles:

- **CLI mode** (`notify`, `watch`, `ssh`) — synchronous commands that send HTTP requests to the Zestful Mac app on `localhost:21547`. No async runtime needed.
- **Daemon mode** (`daemon`) — an async [axum](https://github.com/tokio-rs/axum) server on `localhost:21548` that handles terminal focus switching. Uses the [iterm2-client](https://crates.io/crates/iterm2-client) crate for native iTerm2 tab switching and a built-in workspace inspector for automatic terminal/multiplexer/IDE/browser detection.

```
Agent hook fires
    -> zestful notify (auto-captures terminal URI, POSTs to Mac app on :21547)
    -> Mac app shows overlay, optional push to iPhone
    -> User clicks alert
    -> Mac app POSTs terminal URI to zestful daemon on :21548
    -> Daemon parses URI, switches to correct terminal tab
```

The daemon auto-starts when any CLI command runs.

## How It Works

1. The Zestful Mac app runs a local HTTP server on `localhost:21547`
2. The CLI sends notifications via HTTP POST with an auth token
3. `workspace::locate()` auto-captures a `workspace://` URI identifying the exact terminal/tab/pane
4. The app shows alerts in the floating overlay and menu bar
5. If logged in, alerts forward as push notifications to your iPhone
6. Click any alert to focus the agent's terminal via the focus daemon

## Building

```bash
cargo build --release
cargo test
```

Requires Rust 1.70+. On macOS, the `iterm2-client` crate is included for native iTerm2 support.

## Links

- [Website](https://zestful.dev)
- [FAQ](https://zestful.dev/faq)
- [Privacy Policy](https://zestful.dev/privacy)
- [Contact](https://zestful.dev/contact)

## License

MIT
