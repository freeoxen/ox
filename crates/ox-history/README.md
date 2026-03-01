# ox-history

Conversation history as a StructFS store for the [ox](https://github.com/freeoxen/ox) agent framework.

## What's in the box

- **`HistoryProvider`** — a `Vec<Message>` exposed via the `Reader`/`Writer` interface
- **`parse_wire_message`** — converts Anthropic Messages API JSON into typed `Message` values

## Store paths

| Path | Read | Write |
|------|------|-------|
| `""` / `"messages"` | Wire-format JSON array | — |
| `"count"` | Message count (integer) | — |
| `""` / `"append"` | — | Parse and append a message |
| `"clear"` | — | Clear all messages |

The kernel writes assistant messages and tool results by writing to `path!("history/append")`.

## License

Apache-2.0 — see [LICENSE](../../LICENSE).
