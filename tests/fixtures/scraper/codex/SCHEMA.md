# Codex Session JSONL — Schema Notes

Captured from a real Codex Desktop session at `~/.codex/sessions/<year>/<month>/<day>/rollout-<timestamp>-<uuid>.jsonl` (Codex CLI v0.122.0-alpha.13).

## Top-level line shape

Every line is one JSON object with:

```
{
  "timestamp": "<ISO-8601 with timezone, e.g. 2026-04-20T21:51:42.112Z>",
  "type":      "<one of: session_meta | turn_context | event_msg | response_item>",
  "payload":   { ... }
}
```

The `type` discriminator at top-level tells you the line's category. `payload` is itself an object whose shape depends on `type` (and, for `event_msg`, on `payload.type`).

## Where the parser-relevant fields live

### session_id

Appears **once** at the top of the file in a `session_meta` line:

```json
{
  "type": "session_meta",
  "payload": {
    "id": "019dace0-ba1c-7d83-8cc1-395c89d2199c",
    ...
  }
}
```

The Claude parser reads `sessionId` from every line; the Codex parser must remember this id from the file's header line. Resuming from a non-zero offset must re-scan from the start to recover it.

### model

Appears in `turn_context` lines, one per turn:

```json
{
  "type": "turn_context",
  "payload": {
    "turn_id": "<uuid>",
    "model": "gpt-5.4",
    ...
  }
}
```

Note: `turn_context` precedes the `token_count` event for that turn. The parser needs to track the most recent `turn_context.payload.model` and apply it to subsequent `token_count` lines for the same `turn_id`.

### turn_id

Appears on `turn_context` lines and (sometimes) on `event_msg` lines. The token_count event for a turn is delivered as part of the same logical turn-context window; `turn_id` may not appear directly on the token_count line, so the parser uses the most-recent-seen `turn_id` when the line itself doesn't carry one. If neither path yields a turn_id, fall back to a deterministic hash of the immutable token-usage data (model + input + output + cache_read + reasoning).

### usage / tokens

Appears in **`event_msg`** lines with `payload.type == "token_count"`. Two sub-fields matter:

- `payload.info.last_token_usage` — the per-turn token deltas. **Use this for the `Tokens` struct.**
- `payload.info.total_token_usage` — running totals across the session. Useful for context-window utilization but NOT what we emit as per-turn usage.

```json
{
  "type": "event_msg",
  "payload": {
    "type": "token_count",
    "info": {
      "last_token_usage": {
        "input_tokens": 16124,
        "cached_input_tokens": 11648,
        "output_tokens": 380,
        "reasoning_output_tokens": 87,
        "total_tokens": 16504
      },
      "total_token_usage": { ... },
      "model_context_window": 258400
    }
  }
}
```

Also note the second token_count event in a session can have `info: null` (early in the session, before any tokens have been counted). The parser must skip those.

### Field name mapping (Codex → unified `Tokens`)

| Codex (under `last_token_usage`) | Unified `Tokens` field | Notes |
|---|---|---|
| `input_tokens`            | `input`        | direct |
| `output_tokens`           | `output`       | direct |
| `cached_input_tokens`     | `cache_read`   | Codex tracks read-only cache; no separate write |
| (no field)                | `cache_write`  | always 0 |
| `reasoning_output_tokens` | `reasoning`    | direct |

### timestamp

Top-level `timestamp` field is an ISO-8601 string with `.fffZ` (UTC) suffix. Parse to ms-epoch the same way the Claude parser does.

## Lines the parser must skip

- `response_item` — assistant/user/tool messages, free-text content, no usage.
- `event_msg` with `payload.type` other than `token_count` — chat metadata, thread events, etc.
- `event_msg` with `payload.type == "token_count"` but `payload.info == null` — pre-usage placeholder lines.

## Differences from Claude that matter

1. **`session_id` is in the header**, not on every line. Resume-from-offset must re-read the header.
2. **Schema is `{type, payload}`**, not Claude's flatter `{type, message, ...}`.
3. **No `cache_creation_input_tokens` analog** — Codex tracks only cache reads.
4. **One file per session**, named `rollout-<isoTime>-<uuid>.jsonl`, organized into `~/.codex/sessions/<year>/<month>/<day>/`.

## Reference

- Source fixture captured: 2026-04-20 session, sanitized in `turn_complete.jsonl`.
- Codex CLI source: https://github.com/openai/codex
