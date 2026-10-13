# hearing

The hearing of Emory, version 1: the button ear's server side (`specs/hearing.md`). From the audio
of the ears (`ear.*.audio`) to the sentences (`hearing.sentence`) and the whole turn
(`hearing.turn`), with Silero VAD and Parakeet TDT 0.6B v3 (sherpa-onnx, inside the binary): int8 on
the CPU, fp32 on an NVIDIA GPU (the server's setup, see "Run on an NVIDIA GPU").

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

Parakeet writes the names it does not know as it hears them (« A Mira » for Emory, a product name
cut in two). The vocabulary lists names (one per line, `#` for comments) that it favours when it
hesitates. It is the user's own data, outside the repository: `hotwords.txt` in the data folder
(`$XDG_DATA_HOME/emory`, so `~/.local/share/emory` by default; on the server, a volume), or the
path given in the configuration. `examples/hotwords.txt` is a starting point to copy there.

```toml
[transcription]
# hotwords = "~/.local/share/emory/hotwords.txt"   # the default
hotwords_score = 2.0        # how much a name is favoured; 3 and more invents words
# decoding_method = "modified_beam_search"   # see below
```

- With at least one word, the search is `modified_beam_search`; with an absent or empty file, it
  stays `greedy_search`, which is faster. `decoding_method` forces one or the other
  (`greedy_search` with hotwords is refused).
- **The mind adds words while the hearing runs** (`hearing.vocabulary.add`, a request with a
  reply, see `specs/contract.md`), once the user has said yes. The word is appended to the file
  (once: a word already there, in any case, is a success with nothing written; what was heard
  instead goes on a comment line above it) and used from the next sentence, without a restart:
  the words go to sherpa-onnx with each sentence, which costs about 0.1 ms for 10 words. With the
  greedy search (empty vocabulary at start), the word is written and counts from the next start.
- The words need the model's `bpe.vocab`, which the model does not ship:
  `scripts/download-models.sh` writes it into the Parakeet folder (`bpe_vocab` points elsewhere).
- Only names that are not words of the language: a French word in the list is pulled into
  sentences that do not mean it. A form too far from the name (« Et Mori ») is for the corrections
  table, `corrections.tsv` in the same data folder (`examples/corrections.tsv` to start, which
  also reads « A Mira, » at the start of a sentence as « Emory, »). A table given in the
  configuration must exist; the default one may be absent.
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

Then, in the configuration (the whole server setup is `examples/hearing-server.toml`):

```toml
[models]
parakeet = "~/.cache/emory/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3"   # the fp32 folder
[transcription]
provider = "cuda"          # "cpu" by default
precision = "fp32"         # "int8" or "fp32"; absent: the int8 files of the folder if any, else fp32
fallback_to_cpu = false    # the default, see below
[vad]
provider = "cpu"           # Silero stays here (see below)
```

**On the server all the AI computation runs on the GPU** (decided 2026-10-13): the CPU belongs to
the other services, and serves Emory only as a fallback. **On a development PC it stays on the CPU
with the int8 model** (`examples/hearing.toml`), and the GPU is used only for measures.

### When CUDA cannot be had

- `provider = "cuda"` with a CPU build, or with a missing driver, CUDA or cuDNN library, or no free
  memory on the card, is an **error at start** that says why. This is the default, on purpose: a
  server that falls back to the CPU in silence slows down everything else on the machine and
  nobody knows.
- `fallback_to_cpu = true` loads Parakeet on the CPU instead, and logs a `warn` line, `Parakeet:
  cuda is unavailable, FALLING BACK TO THE CPU`, with the cause; `Parakeet ready` then gives the real
  `provider`. The fp32 model on the CPU is 2.3 times slower than the int8 one (523 ms against 205
  ms a sentence); for a quick CPU, configure int8 on the CPU, not this.
- After the load, `hearing` checks that onnxruntime really loaded its CUDA provider
  (`libonnxruntime_providers_cuda` in the process, Linux only): asking for CUDA does not guarantee
  getting it. This check has not yet run on a real card (see `specs/hearing.md`, "à mesurer").
- `precision` is followed to the letter when given: a missing file is an error naming the file,
  never another model.

### The fp32 model

`EMORY_FP32=1 hearing/scripts/download-models.sh` adds the full-precision Parakeet beside the int8
one (2.5 GB from Hugging Face; run it again after an interrupted download). It goes into
`~/.cache/emory/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3`, with its `bpe.vocab`: the files
`encoder.onnx`, `encoder.weights` (must stay beside the encoder), `decoder.onnx`, `joiner.onnx` and
`tokens.txt`.

### Measuring CPU against GPU

`scripts/measure-gpu.sh` measures, for recorded sentences, int8 CPU, fp32 CPU and fp32 CUDA (greedy
and beam + the vocabulary), and Silero on the CPU against CUDA: times (load, warm-up, per sentence),
CPU used, VRAM of the process, differences in the words. It builds a Markdown table.

```sh
hearing/scripts/build-cuda.sh                        # also builds hearing-measure
hearing/scripts/measure-gpu.sh --dry-run a.wav       # the plan, nothing runs
hearing/scripts/measure-gpu.sh --cpu-only a.wav      # the CPU rows only, no GPU
hearing/scripts/measure-gpu.sh --owner-ok a.wav b.wav   # all of it: uses the GPU
```

`--owner-ok` is for when the owner has agreed and freed the card (on the development PC, stopped
voxtype); the script refuses to start if voxtype runs or the card has less than 4500 MiB free. The
recordings are read in place (16 kHz mono 16-bit) and never leave the machine; the results folder
(`~/.local/state/emory/measure-gpu/<date>` by default) holds their texts, so it stays out of the
repository. `hearing-measure run|table` is the program behind it (one setting per process).

Which model, which unit (measured on 2026-10-12, see `specs/hearing.md`):

- **Parakeet int8 on the GPU gains nothing**: onnxruntime keeps the quantized operations on the CPU
  (the CPU time used stays at 93 % of a CPU run) and the speed is the same, or lower with the
  hotwords. The GPU wants the full-precision model (above). **The fp32 model on the GPU has not
  been measured yet**: that is what `measure-gpu.sh` is for.
- **Silero stays on the CPU** (`[vad] provider = "cpu"`, the default): on the GPU it is 7 times
  slower (25 ms of compute per second of audio against 3.6).
- The GPU memory is shared with the other models of the machine: the int8 model holds about
  0.75 GB, the fp32 one needs more than 2.4 GB for its weights alone (about 3 GB, to be measured).
- The words are not identical to the CPU's: a few words per twenty sentences change with the
  quantized operations, neither better nor worse.
