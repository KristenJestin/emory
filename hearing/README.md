# hearing

The hearing of Emory, version 1: the button ear's server side (`specs/hearing.md`). From the audio
of the ears (`ear.*.audio`) to the sentences (`hearing.sentence`) and the whole turn
(`hearing.turn`), with Silero VAD and Parakeet TDT 0.6B v3 int8 (sherpa-onnx, inside the binary).

```sh
hearing/scripts/download-models.sh               # models in ~/.cache/emory/models/
docker compose -f dev/compose.yml up -d           # the development NATS
cargo run --release -p hearing -- hearing/examples/hearing.toml
```

End-to-end try, in another terminal (the file must be 16 kHz mono 16-bit:
`ffmpeg -i input.wav -ar 16000 -ac 1 -c:a pcm_s16le output.wav`):

```sh
cargo run --release -p hearing --bin hearing-replay -- output.wav
```

`hearing-replay` plays the file like an ear (`start`, 20 ms chunks at real-time pace, `end`),
prints the sentences and the turn that come back, and the time from the end to the turn.

The logs (`RUST_LOG=info`) give, per sentence, its duration and its transcription time, and per
turn the time from the `end` to the published `Turn`. They never contain the text said.
