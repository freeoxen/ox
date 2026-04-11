# ox-history

Conversation history as a StructFS store for the [ox](https://github.com/freeoxen/ox) agent framework.

## What's in the box

- **`HistoryView`** — projects conversation history from a `SharedLog` into wire-format messages
- **`TurnState`** — ephemeral per-turn UI state (streaming text, thinking status, tool status)

## Store paths

| Path | Read | Write |
|------|------|-------|
| `""` / `"messages"` | Wire-format JSON array | — |
| `"count"` | Message count (integer) | — |
| `"append"` | — | Convert wire message to LogEntry, append to SharedLog |
| `"turn/{streaming,thinking,tool,tokens}"` | Ephemeral turn state | Update turn state |
| `"commit"` | — | Finalize streaming text into committed assistant message |

The log is the source of truth. History is a derived view — the kernel writes to `log/append`, and `HistoryView` projects log entries into wire-format messages for prompt synthesis.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).
