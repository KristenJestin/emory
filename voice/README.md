# voice

The voice of Emory, version 1 (`specs/voice.md`): receives the sentences to say (`voice.say`),
has them synthesised by the engine (Pocket on the processor, Kyutai TTS 1.6B on the graphics
card) and sends the audio back to the mouth (`mouth.<name>.audio`), chunk by chunk.

```sh
docker compose -f dev/compose.yml up -d          # the development NATS and Pocket
cargo run -p voice -- dev/voice.toml             # from the repository root
cargo run -p voice --bin voice-try -- "Bonjour."  # says a sentence, writes voice.wav
```

## Configuration

`voice <file.toml>`: see `dev/voice.toml` and the commented list of keys at the top of
`src/config.rs`. A value out of its range stops the start with a message that names the key.

### The loudness of the voice: `gain`

```toml
gain = 2.0    # 0.1 to 8, default 1
```

A factor on the amplitude of what the engine says, whatever the engine: 1 sends the audio as it
comes, 2 doubles it (about +6 dB), 0.5 halves it. It applies chunk by chunk as the audio streams
(no wait, no added latency) and does not touch the waiting cue, which has its own `[cue] volume`.

Up to 0.8 of full scale a sample is only multiplied. Past that the curve bends smoothly towards
full scale (a `tanh` that starts with the same slope), so a loud syllable is rounded, never cut:
a hard clip would crackle. With a gain of exactly 1 the bytes are left as the engine sent them.
Pocket peaks around 0.3 to 0.4 of full scale as it comes: `gain = 2.0` brings that to 0.6 to 0.9.
