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
```

| Flag | Required | Description |
|------|----------|-------------|
| `--agent` | Yes | Agent name (e.g. `claude-code`, `cursor`) |
| `--message` | Yes | Message to display |
| `--severity` | No | `info`, `warning` (default), or `urgent` |
| `--app` | No | App to focus when alert is clicked |
| `--window-id` | No | Window ID for focus |
| `--tab-id` | No | Tab ID for focus |

### Severity Levels

| Level | Overlay | Menu Bar |
|-------|---------|----------|
| `info` | Returns to "All Clear" (green) | Badge clears |
| `warning` | Pulses amber | Badge shows count |
| `urgent` | Flashes red | Badge shows count |

## Agent Hooks

### Claude Code

Add to `.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "zestful notify --agent \"claude-code:$(basename $PWD)\" --message 'Waiting for your input' --app \"$TERM_PROGRAM\""
      }]
    }],
    "Start": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "zestful notify --agent \"claude-code:$(basename $PWD)\" --message 'Working...' --severity info"
      }]
    }]
  }
}
```

### Cursor

```bash
zestful notify --agent "cursor" --message "Cursor needs input" --app "Cursor"
```

### Aider

```bash
aider "$@"; zestful notify --agent "aider:$(basename $PWD)" --message "Aider finished" --app "$TERM_PROGRAM"
```

### Any Script

```bash
zestful notify --agent "deploy" --message "Deploy needs approval" --severity warning
zestful notify --agent "ci" --message "Build failed!" --severity urgent
```

## Click-to-Foreground

Pass `--app` to bring the agent's terminal to the front when you click the alert:

```bash
zestful notify --agent "test" --message "waiting" --app "$TERM_PROGRAM"
```

Works with Kitty, Terminal.app, iTerm2, VS Code, Cursor, and any app via AppleScript. Configure the focus method in Settings.

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
