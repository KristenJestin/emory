# mind

The mind of Emory, version 1 (`specs/mind.md`): from what the ears heard (`hearing.sentence`,
`hearing.turn`, the presses of `ear.<name>.button`) to the answer said sentence by sentence
(`voice.say`, `voice.stop`), with the LLM behind an ACP adapter and the memory in Hippocampe.

```sh
docker compose -f dev/compose.yml up -d           # the development NATS and voice engine
cargo run --release -p mind -- dev/mind.toml      # from the root of the repository
```

The chain on the PC, each in its terminal: `hearing`, `voice`, `mind`, `emory-mouth`, and an ear
(`emory-ear`, or `hearing-replay file.wav` to play a recording like an ear).

- **The LLM**: `mind` starts the adapter (`[llm] command` and `args`, `claude-agent-acp` by
  default), one ACP session per conversation, with Claude Code stripped of all its own tools,
  settings and skills (`_meta` of `session/new`, see `src/acp.rs`). The adapter inherits the
  environment: the local Claude Code login in development, `CLAUDE_CODE_OAUTH_TOKEN` when set.
  Every request takes from the Claude subscription.
- **The instructions** of the LLM are in `prompts/system.fr.md`: two or three short sentences by
  default, longer only when Kris asks for details.
- **A turn with nothing in it** (empty, or only blanks and punctuation: the headset sent
  near-silence) gets no request to the LLM: Emory says `[conversation] nothing_heard` (default
  « Je n'ai rien entendu. ») with `voice.say` and `last`, to the device of the turn.
- **Emory's MCP server** is served by `mind` itself, on the loopback, with a key drawn at
  start-up and given only to the adapter; it is the only MCP server the LLM gets. Its tools are
  modules (`src/tools.rs`): for now Hippocampe's tools, relayed as `hippocampe_<name>`.
- **The memory**: one connection to Hippocampe kept open. Each sentence heard is searched as it is
  (top 5, given up beyond 300 ms); at the end of the turn, the title, summary and excerpt of the
  entries found go into the request, with what Hippocampe flags for the day (`briefing`) in the
  first request of a conversation.

The logs (`RUST_LOG=mind=info`) give the times of each turn (request sent, first word of the LLM,
first sentence sent to the voice, end) and each tool call. They never contain what was said. The
dependencies that would log the messages themselves (the ACP client, MCP, the HTTP stack) stay at
`warn` whatever `RUST_LOG` says.

Tests: `cargo test -p mind`, with a fake LLM, a fake ACP agent and a fake Hippocampe; none of them
reaches the real Hippocampe or spends a request.
