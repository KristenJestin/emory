# Kyutai TTS 1.6B, the voice engine on the graphics card

Kyutai TTS 1.6B (`kyutai/tts-1.6b-en_fr`, CC-BY-4.0) through the Python `moshi` package (0.2.13),
behind `serve.py`, a small HTTP server of ours. The voice is the one Kris chose on 2026-10-12,
`cml-tts/fr/5476_3103_000072-0001.wav` from `kyutai/tts-voices`, with `padding_bonus = -3`.

The API, the same shape as Pocket's so that `voice` handles both the same way:

- `GET /health`: 200 once the model is loaded and warm;
- `POST /tts` with a JSON object `{"text", "voice"?, "padding_bonus"?, "max_padding"?, "temp"?,
  "cfg_coef"?}` (the defaults are Kris's settings): a mono 16-bit WAV at 24 kHz, streamed frame by
  frame (80 ms) as it is computed. A bad request is a 400 with `{"error"}`.

One sentence at a time. Closing the connection stops the computation at the next step (about
15 ms on an RTX 3070 Ti).

| Variable | Default | |
|---|---|---|
| `KYUTAI_HOST` | `127.0.0.1` (`0.0.0.0` in the image) | |
| `KYUTAI_PORT` | `8001` | |
| `KYUTAI_DEVICE` | `cuda` | `cpu` works, far slower than real time |
| `KYUTAI_N_Q` | `32` | codebooks of the audio; fewer is faster and sounds worse |
| `KYUTAI_THREADS` | `4` | torch threads on the processor |
| `KYUTAI_LOG` | `INFO` | `DEBUG` adds the step at which the first sound is computed |

The settings of a sentence come with each request, from the configuration of `voice`
(`[kyutai]`); only what changes the loaded model is in the environment.

## Why this server, and not Kyutai's

Kyutai's server (`moshi-server`, in Rust) runs this model through the same Python code: its TTS
module embeds `moshi` with pyo3, and only Mimi runs in Rust. Using it means building it with
`--features cuda` (candle and its CUDA kernels) and also providing that Python environment. It
exposes everything Kris's voice needs (voice, `padding_bonus`, `max_padding`, `temp`,
`cfg_coef`) and takes the text word by word over a websocket, with batching for several
clients. For one sentence at a time, `serve.py` gives the same streaming with one dependency.

What it keeps from `moshi-server`: one generator, built once and reset before each sentence
(building one captures its CUDA graphs again, which cost about half a second per sentence).

## Run

On a machine with an NVIDIA card, without Docker, from a venv with `moshi`:

```
python -m venv venv && venv/bin/pip install torch==2.9.1 --index-url https://download.pytorch.org/whl/cu128
venv/bin/pip install moshi==0.2.13
KYUTAI_VENV=venv engines/kyutai/run-native.sh
```

With Docker, once the host has the NVIDIA container runtime:

```
docker build -t emory-kyutai engines/kyutai
docker run --gpus all -p 127.0.0.1:8001:8001 -v kyutai-cache:/cache emory-kyutai
```

The weights (about 4 GB) and the voice are downloaded on first start into the Hugging Face
cache. The process holds about 4.5 GB of video memory.

```
curl -s http://127.0.0.1:8001/tts -H 'content-type: application/json' \
  -d '{"text": "Bonjour Kris."}' -o hello.wav
```
