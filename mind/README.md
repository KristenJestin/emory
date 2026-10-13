# mind

The mind of Emory, version 1 (`specs/mind.md`): from what the ears heard (`hearing.sentence`,
`hearing.turn`, the presses of `ear.<name>.button`) to the answer said sentence by sentence
(`voice.say`, `voice.stop`), with the LLM behind an ACP adapter or a provider's API, and the
memory in Hippocampe.

```sh
docker compose -f dev/compose.yml up -d           # the development NATS and voice engine
cargo run --release -p mind -- dev/mind.toml      # from the root of the repository (ACP)
cargo run --release -p mind -- dev/mind-api.toml  # the same, straight through the API
```

The chain on the PC, each in its terminal: `hearing`, `voice`, `mind`, `emory-mouth`, and an ear
(`emory-ear`, or `hearing-replay file.wav` to play a recording like an ear).

- **The LLM**, chosen by `[llm] mode` (`src/backend.rs`), the rest of `mind` does not know which:
  - `mode = "acp"` (the default): `mind` starts the adapter (`command` and `args`,
    `claude-agent-acp` by default), one ACP session per conversation, with Claude Code stripped of
    all its own tools, settings and skills (`_meta` of `session/new`, see `src/acp.rs`). The
    adapter inherits the environment: the local Claude Code login in development,
    `CLAUDE_CODE_OAUTH_TOKEN` when set. Every request takes from the Claude subscription.
  - `mode = "api"`: `mind` calls the provider's API itself with `genai` (`src/api.rs`):
    `provider` (`anthropic`, `openai`, `gemini`, `groq`, `mistral`, `cerebras`,
    `openai_compatible` with `base_url`…), `model`, `key_file` (read at each request, never
    logged). The answer streams; the tools the model asks for run in the process, through the
    same modules as the MCP server, with the conversation's key; the history of a conversation is
    kept by `mind`; a barge-in drops the connection at once. With Anthropic, the instructions, the
    tools and the end of the history are cached (`cache`, on by default).
  - Both: `model`, `effort`, `thinking` (`"off"` by default, `"adaptive"`, or a budget of
    tokens), `fast` (Anthropic's fast mode, Opus only, billed at a premium: read
    `specs/mind.md` first). An option the mode cannot honour stops `mind` at start-up with its
    name.
- **The instructions** of the LLM are in `prompts/system.fr.md`: two or three short sentences by
  default, never cut short, longer when asked for details. The file is a template, filled from the
  `[owner]` section of the configuration: `{owner}` becomes `name` (« la personne qui te parle »
  without it) and `{address}` becomes `address`, the free text on how to speak to the owner (« un
  homme ; tu le tutoies »; without it, « tu tutoies cette personne »). The shipped text names
  nobody, and neither do the markers of the requests (`[On te dit]`, see `src/request.rs`).
- **A turn with nothing in it** (empty, or only blanks and punctuation: the headset sent
  near-silence) gets no request to the LLM: Emory says `[conversation] nothing_heard` (default
  « Je n'ai rien entendu. ») with `voice.say` and `last`, to the device of the turn.
- **Emory's MCP server** (ACP mode) is served by `mind` itself, on the loopback, with a key drawn at
  start-up and given only to the adapter; it is the only MCP server the LLM gets. Its tools are
  modules (`src/tools.rs`): Hippocampe's tools, relayed as `hippocampe_<name>`, and the
  dictionary. Each conversation draws a key that its session gives the server in the
  `emory-conversation` header, so a tool knows which conversation calls it and the user's last
  turn in it (`src/conversations.rs`).
- **The dictionary** (`src/dictionary.rs`, `[dictionary]`): `dictionary_propose(word, heard_as?)`
  notes a word in the conversation and tells the LLM to ask the user; `dictionary_add(word)` is
  accepted only for a word proposed in an earlier turn of the same conversation (the user has
  spoken since), then asks the hearing on `hearing.vocabulary.add` (`timeout_ms`, 2 s by
  default). The rule is held by `mind`, not left to the LLM; the proposals die with the
  conversation. `enabled = false` takes the two tools away.
- **The web** (`src/web/`, `[web]`, `specs/web.md`): the module `web`, with one HTTP client kept
  open for its tools. `web_weather(place?, days?)`: the weather now and in the next days (1 to 7, 2 by
  default) from Open-Meteo, for the place asked or the owner's (`[web.weather] place`, a name found
  by Open-Meteo: `"Brest, France"` when the name is shared by cities of several countries;
  optionally `latitude` and `longitude`), in whole degrees, the sky in French words (the WMO codes
  are in `prompts/weather.fr.toml`) and the days named; a forecast is kept `cache_minutes`.
  `web_wikipedia(query)`: searches the title on Wikipedia (French
  first, then English, `[web.wikipedia] languages`), gives the summary of the first one without its
  pronunciation, cut to `max_chars`, or the other titles found when the page is a disambiguation
  page. Failures are said to the LLM in one sentence. No key, no account; `[web] user_agent` is
  where the owner puts a contact for Wikimedia. `enabled = false` takes the tools away. The logs
  give the host, the status, the size and the time of each request, never the question. The tests
  serve recorded answers (`tests/fixtures/web/`); `cargo test -p mind --test web real_wikipedia --
  --ignored --nocapture` (or `real_open_meteo`) asks the real service, by hand.
- **The memory**: one connection to Hippocampe kept open. Each sentence heard is searched as it is
  (top 5, given up beyond 300 ms); at the end of the turn, the title, summary and excerpt of the
  entries found go into the request, with what Hippocampe flags for the day (`briefing`) in the
  first request of a conversation.

The logs (`RUST_LOG=mind=info`) give the times of each turn (request sent, first word of the LLM,
first sentence sent to the voice, end) and each tool call. They never contain what was said. The
dependencies that would log the messages themselves (the ACP client, `genai`, MCP, the HTTP stack) stay at
`warn` whatever `RUST_LOG` says.

Tests: `cargo test -p mind`, with a fake LLM, a fake ACP agent, a fake Anthropic API on the
loopback and a fake Hippocampe; none of them reaches the real Hippocampe or spends a request.

The talker bench (`examples/talker-bench.rs`, with `dev/talker-bench.toml`) sends the same request
straight to the Anthropic API with `genai`, running the tools in-process, and records the times and
the cost of each answer; `examples/talker-probe.rs` plays turns on a bus of its own (press,
sentence, turn, and a barge-in for a question written `!question`) to time a running `mind`,
whichever its mode. Neither changes how `mind` answers. Results: `specs/mind.md`.
