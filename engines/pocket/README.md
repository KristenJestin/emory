# Pocket TTS, the voice engine of the voice

Kyutai Pocket TTS (`pocket-tts` 3.3.0), `french` model (6 layers), `estelle` voice. The server is
the library's own (`pocket-tts serve`); `serve.py` only starts it with the settings from the
environment, because the library pins the number of torch threads to 1 itself and we want to be
able to change it.

| Variable | Default | |
|---|---|---|
| `POCKET_LANGUAGE` | `french` | the model (`french_24l`: too slow without a graphics card) |
| `POCKET_VOICE` | `estelle` | the default voice |
| `POCKET_THREADS` | `1` | the torch threads |
| `POCKET_PORT` | `8000` | |

The API: `GET /health`, and `POST /tts` (form with `text`, optional `voice_url`) which returns a
mono 16-bit WAV at 24 kHz streamed while it is computed. The model and the voice state are loaded
once at start. The weights are downloaded on first start into `/cache` (`HF_HOME`): mount a volume
there. Closing the connection stops the computation.

```
docker compose -f dev/compose.yml up -d     # the bus and the engine, on 127.0.0.1
curl -s -X POST http://127.0.0.1:8000/tts -F 'text=Bonjour.' -o hello.wav
```
