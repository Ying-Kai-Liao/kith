# kith-fakemodel

A fake Anthropic Messages API, so real Claude Code runs in tests with no API key. Plain std
networking: it builds and runs on Windows too.

```
cargo build --manifest-path serve/host/Cargo.toml -p kith-fakemodel
kith-fakemodel --script turns.json [--log requests.jsonl] [--addr 127.0.0.1:0]
```

It prints the bound address (`127.0.0.1:PORT`) as its first stdout line, then serves until killed.
`--log` appends one JSON line per request: `{"method","path","body"}` (body parsed, so a test can
assert on the `tool_result` Claude Code sent back).

Run Claude Code against it with a throwaway config dir, never your own:

```
ANTHROPIC_BASE_URL=http://127.0.0.1:PORT ANTHROPIC_API_KEY=dummy CLAUDE_CONFIG_DIR=<temp dir> \
  claude -p "say hi" --model sonnet
```

## Script

```json
{
  "turns": [
    {"text": "Running it.", "tool_use": [{"name": "Bash", "input": {"command": "echo hi", "description": "say hi"}}]},
    {"match": "hi", "text": "It printed hi."}
  ],
  "fallback": "Done."
}
```

- Each `/v1/messages` request takes the first unused turn. A turn with `match` is only taken when
  that substring is in the last user message (its text or `tool_result` content); otherwise it is
  skipped and stays for a later request.
- A turn has `text` and/or `tool_use` (a list of `{name, input}`: Bash, Write, Read, any tool) and
  `stop_reason` (default `tool_use` when there are tool uses, else `end_turn`).
- When the script runs out the reply is `fallback` (default `Done.`) with `end_turn`.
- Requests for a haiku model (Claude Code's title and summary calls) get `{}` and never use a turn.
  `count_tokens`, `GET /v1/models` and `HEAD /` are answered. Nothing returns 5xx unless scripted.
- `"stream": true` gets SSE; otherwise one JSON message.
