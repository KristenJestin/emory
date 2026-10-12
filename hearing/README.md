# hearing

The hearing of Emory, version 1: the button ear's server side (`specs/hearing.md`). From the audio
of the ears (`ear.*.audio`) to the sentences (`hearing.sentence`) and the whole turn
(`hearing.turn`), with Silero VAD and Parakeet TDT 0.6B v3 int8 (sherpa-onnx, inside the binary).

```sh
hearing/scripts/download-models.sh               # models in ~/.cache/emory/models/
docker compose -f dev/compose.yml up -d           # the development NATS
cargo run --release -p hearing -- hearing/examples/hearing.toml   # from the repository root
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

## Configuration

`hearing <file.toml>`: a relative path in the file (models, corrections, hotwords, certificates) is
read from the folder of the file, wherever the program is started. `~/` is the home folder. See
`examples/hearing.toml`.

## Is the microphone sending anything? (`RUST_LOG=hearing=debug`)

At `debug`, each turn gets a `turn level` line: the `audio_ms` received, its `peak_dbfs` and
`rms_dbfs` (0 is the loudest possible; speech close to the microphone peaks around -20 to -3, a
muted or dying headset sits below -50), the `speech_ms` and `segments` the VAD found, and the
`sentences` that came out. A turn of more than half a second that ends with no sentence adds a
`no sentence in the turn` line: `no voice found` (the VAD saw nothing: look at the levels) or `voice
found, but nothing came out of it` (the VAD saw speech but Parakeet gave no text). `emory-ear` logs
the same levels, at `debug` too, for what its microphone captured: if they differ, the sound was
lost on the way.

## Vocabulary (hotwords)

Parakeet writes the names it does not know as it hears them (« A Mira » for Emory, « Omarchi »,
« ChrisNet »). `examples/hotwords.txt` lists names (one per line, `#` for comments) that it favours
when it hesitates:

```toml
[transcription]
hotwords = "hotwords.txt"
hotwords_score = 2.0        # how much a name is favoured; 3 and more invents words
# decoding_method = "modified_beam_search"   # see below
```

- With at least one word, the search is `modified_beam_search`; with an absent or empty file, it
  stays `greedy_search`, which is faster. `decoding_method` forces one or the other
  (`greedy_search` with hotwords is refused).
- The words need the model's `bpe.vocab`, which the model does not ship:
  `scripts/download-models.sh` writes it into the Parakeet folder (`bpe_vocab` points elsewhere).
- Only names that are not words of the language: a French word in the list is pulled into
  sentences that do not mean it. A form too far from the name (« Et Mori ») is for the corrections
  table, whose `examples/corrections.tsv` also reads « A Mira, » at the start of a sentence as
  « Emory, ».
- The measures (what it fixes, what it costs) are in `specs/hearing.md`.

## Run on an NVIDIA GPU (CUDA)

The CPU build stays the default and needs nothing from CUDA. The CUDA build is a cargo feature,
`cuda`: it links sherpa-onnx and onnxruntime as shared libraries (the CUDA archive of the
sherpa-onnx release) instead of the CPU static ones the crate downloads.

```sh
hearing/scripts/build-cuda.sh        # CUDA 13 (the default); `build-cuda.sh 12` for a CUDA 12 machine
```

It downloads the archive once (254 MB, into `~/.cache/emory/sherpa-onnx-cuda/13/`), builds with
`--no-default-features --features cuda` and `SHERPA_ONNX_LIB_DIR`, and leaves the libraries beside
`target/release/hearing` and `hearing-replay`, which find them by themselves (`rpath` `$ORIGIN`):
copy the binary with the `lib*.so` files that sit next to it. The CPU and the CUDA build share
`target/`: give the CUDA one its own with `CARGO_TARGET_DIR=target-cuda` to keep both.

On the machine, nothing to compile and nothing installed by the script: the NVIDIA driver, the CUDA
runtime of the same major as the archive (`libcudart`, `libcublas`, `libcublasLt`, `libcurand`) and
cuDNN 9 (`libcudnn.so.9`), all found by the dynamic linker (on Arch: the `cuda` and `cudnn`
packages; `ldconfig -p | grep -E 'cudnn|cudart|cublas|curand'` must list them). Checked with CUDA
13.3 and cuDNN 9.25 on an RTX 3070 Ti, driver 610.

Then, in the configuration:

```toml
[transcription]
provider = "cuda"      # "cpu" by default
[vad]
provider = "cpu"       # Silero stays here (see below)
```

`provider = "cuda"` with a CPU build is a start-up error (not a silent fall back to the CPU). The
`Parakeet ready` line at `info` says the provider, the load time and the warm-up time.

Which model, which unit (measured on 2026-10-12, see `specs/hearing.md`):

- **Parakeet int8 on the GPU gains nothing**: onnxruntime keeps the quantized operations on the CPU
  (the CPU time used stays at 93 % of a CPU run) and the speed is the same, or lower with the
  hotwords. The GPU wants the full-precision model: `EMORY_FP32=1 scripts/download-models.sh` puts
  it in `~/.cache/emory/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3` (2.5 GB, with its
  `bpe.vocab`); `parakeet` points to that folder. The program takes `encoder.int8.onnx` when the
  folder has it, else `encoder.onnx` (same for the decoder and the joiner).
- **Silero stays on the CPU** (`[vad] provider = "cpu"`, the default): on the GPU it is 7 times
  slower.
- The GPU memory is shared with the other models of the machine: the int8 model holds about
  0.75 GB, the fp32 one needs more than 2.4 GB for its weights alone.
- The words are not identical to the CPU's: a few words per twenty sentences change with the
  quantized operations, neither better nor worse.
