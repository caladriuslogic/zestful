# Zestful

> Never miss when your AI agent needs you.

Zestful alerts you when Claude Code, Cursor, Aider, or any AI coding agent is waiting for input — floating overlay on Mac, push notifications on iPhone, click to focus the agent's terminal tab.

**Website:** [zestful.dev](https://zestful.dev)

## Install

### CLI (Homebrew)

```bash
brew install caladriuslogic/tap/zestful
```

### CLI (Manual)

```bash
curl -sL https://github.com/caladriuslogic/zestful/releases/latest/download/zestful -o /usr/local/bin/zestful
chmod +x /usr/local/bin/zestful
```

### Mac & iOS App

Download from the [App Store](https://zestful.dev) (coming soon).

## Quick Start

1. Install the Zestful Mac app
2. Install the CLI: `brew install caladriuslogic/tap/zestful`
3. Add the hook to your agent (see below)
4. That's it — the overlay flashes when your agent needs you

## Usage

```bash
zestful notify --agent <name> --message <msg> [options]
zestful watch <command> [args...]
```

| Flag | Required | Description |
|------|----------|-------------|
| `--agent` | Yes | Agent name (e.g. `claude-code`, `cursor`) |
| `--message` | Yes | Message to display |
| `--severity` | No | `info`, `warning` (default), or `urgent` |
| `--app` | No | App to focus when alert is clicked |
| `--window-id` | No | Window ID for focus |
| `--tab-id` | No | Tab ID for focus |
| `--no-push` | No | Suppress push notification for this event |

### `zestful watch`

Wraps any command and notifies when it finishes:

```bash
zestful watch npm run build        # notifies on completion
zestful watch cargo test --release  # notifies on success or failure
zestful watch --agent deploy ./deploy.sh
```

Exit 0 → `warning` ("done"). Non-zero → `urgent` ("failed"). Auto-detects `$TERM_PROGRAM` for click-to-focus.

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
        "command": "zestful notify --agent \"claude-code:$(basename $PWD)\" --message 'Waiting for your input' --app \"$TERM_PROGRAM\" ${KITTY_WINDOW_ID:+--window-id \"$KITTY_WINDOW_ID\"}"
      }]
    }],
    "Start": [{
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
aider --notifications-command 'zestful notify --agent "aider:$(basename $PWD)" --message "$AIDER_NOTIFICATION_TITLE" --app "$TERM_PROGRAM"'
```

### Cursor

Place `.cursor/hooks.json` in your project root (beta):

```json
{
  "hooks": [
    { "event": "stop", "command": "zestful notify --agent \"cursor:$(basename $PWD)\" --message 'Waiting for your input' --app Cursor" },
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

### `zestful ssh`

SSH into a remote box with Zestful forwarding. Agents running on the remote machine will notify your local Mac app.

```bash
zestful ssh dev@myserver.com
zestful ssh dev@myserver.com -p 2222 -i ~/.ssh/mykey
```

This copies your auth token to the remote, sets up a reverse port forward, and opens an SSH session. On the remote, `zestful notify` and `zestful watch` work as if you were local. You can also set this up manually:

```bash
# 1. Copy token to remote
scp ~/.config/zestful/local-token dev@myserver.com:~/.config/zestful/local-token

# 2. SSH with reverse port forward
ssh -R 21547:localhost:21547 dev@myserver.com
```

## Click-to-Foreground

Pass `--app` to bring the agent's terminal to the front when you click the alert:

```bash
zestful notify --agent "test" --message "waiting" --app "$TERM_PROGRAM"
```

Works with Kitty, iTerm2, WezTerm, Terminal.app (tab-level), and VS Code, Cursor, Alacritty, Ghostty, Warp, Hyper (window-level) via AppleScript.

## How It Works

1. The Zestful Mac app runs a local Unix socket server at `/tmp/zestful.sock`
2. The CLI sends notifications to this socket
3. The app shows them in the floating overlay and menu bar
4. If logged in, alerts forward as push notifications to your iPhone
5. Click any alert to focus the agent's window

## Links

- [Website](https://zestful.dev)
- [FAQ](https://zestful.dev/faq)
- [Privacy Policy](https://zestful.dev/privacy)
- [Contact](https://zestful.dev/contact)

## License

MIT
