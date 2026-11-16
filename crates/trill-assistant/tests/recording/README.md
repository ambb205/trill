# Watching the CLI without asking a model

Most of what this crate needs to know about the CLI can be learned without
spending anybody's usage, and was. Two ways, both with the real, unmodified
CLI:

**Signed out.** With `CLAUDE_CONFIG_DIR` pointing at an empty scratch folder
nobody is signed in, the CLI still writes its first line (which lists the
tools it offers) and then says it is not logged in. That is enough to learn
which tools a version has and what a flag does to the list:

```sh
echo hello | CLAUDE_CONFIG_DIR=/tmp/scratch-config claude -p --output-format stream-json --verbose \
    --setting-sources "" --strict-mcp-config --tools "TaskCreate,TodoWrite,WebFetch,WebSearch"
```

**Answering to a stand-in.** `model_stand_in.py` is a few lines of HTTP that
speak enough of the model service's protocol for the CLI to run a turn
against it. It replies from a script (which tools to call, with what, and
how large to say the conversation has grown) and writes down every request
the CLI makes. The CLI and its tools are real; no model is asked and nothing
leaves the machine except what a web tool is told to fetch.

```sh
python model_stand_in.py 18431 script.json records/ &
python waiting_mcp_server.py 18432 records/mcp.jsonl &   # or a real: trill-mcp --http 127.0.0.1:18432 --scratch
echo "plan a song" | CLAUDE_CONFIG_DIR=/tmp/scratch-config \
    ANTHROPIC_API_KEY=dummy-key-for-a-local-stand-in ANTHROPIC_BASE_URL=http://127.0.0.1:18431 \
    claude -p --output-format stream-json --verbose --include-partial-messages \
    --mcp-config mcp.json --strict-mcp-config --setting-sources "" --disable-slash-commands \
    --tools "TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite,WebFetch,WebSearch" \
    --allowedTools "mcp__trill__*,TaskCreate,TaskGet,TaskList,TaskUpdate,TodoWrite,WebFetch,WebSearch" \
    --permission-mode dontAsk --model=haiku > recording.jsonl
```

A script is a JSON list of replies, used in order:

```json
[
  {"blocks": [{"text": "Planning."},
              {"tool": "TaskCreate", "input": {"subject": "Write the drums", "description": "House"}}]},
  {"blocks": [{"tool": "mcp__trill__wait", "input": {"seconds": 75}}], "input_tokens": 985000},
  {"blocks": [{"text": "Done."}]}
]
```

`input_tokens` is how large the stand-in says the conversation is, which is
how the CLI was got to summarise one (`compacted.jsonl`). `waiting_mcp_server.py`
is an MCP server with one tool that waits, for finding how long the CLI lets
a tool call take, and it writes down the requests it gets, for seeing what a
web tool sends to an address on this machine.

The crate's own example client runs the same way, which exercises this crate
and not only the CLI: start `trill-mcp --http`, the stand-in, and

```sh
CLAUDE_CONFIG_DIR=/tmp/scratch-config ANTHROPIC_API_KEY=dummy-key-for-a-local-stand-in \
    ANTHROPIC_BASE_URL=http://127.0.0.1:18431 \
    cargo run -p trill-assistant --example chat -- --keep-environment --mcp http://127.0.0.1:7777/assistant \
    --turns-per-run 3
```

`clean_recording.py` turns a recording into a fixture for `tests/pipeline.rs`:
folders, ids and times particular to the machine are replaced, and it refuses
to write a fixture that still names whoever recorded it.

What this cannot show is what a real model does: whether it keeps its plan,
how it takes the note that carries a request on, what it makes of a page.
`tests/real_cli.rs` is for that, and each run of it is somebody's usage.

None of this touches a login. The scratch folder has none, and the key given
to the stand-in is a made-up word.
