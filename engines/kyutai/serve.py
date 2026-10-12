"""Kyutai TTS 1.6B (`kyutai/tts-1.6b-en_fr`) behind a small HTTP server, on the graphics card.

    POST /tts   JSON {"text", "voice"?, "padding_bonus"?, "max_padding"?, "temp"?, "cfg_coef"?}
                -> a mono 16-bit WAV at 24 kHz, streamed frame by frame (80 ms) as it is computed
    GET /health -> 200 once the model is loaded and warm

One sentence at a time (a lock): the voice sends one sentence at a time anyway. Closing the
connection stops the computation at the next step. The sentence settings come with each request
(the voice keeps them in its configuration); what changes the loaded model (`KYUTAI_N_Q`, the
device, the dtype) comes from the environment.

The generation loop is `TTSModel.generate` of `moshi` for one sentence, with the frames decoded
by Mimi as they come instead of at the end, the way `simple_generate` cuts them (the first
`delay_steps` frames are not sound, the first two decoded ones click, nothing after `end_step`).
"""
import dataclasses
import json
import logging
import os
import select
import socket
import struct
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import torch
from moshi.models.lm import LMGen
from moshi.models.loaders import CheckpointInfo
from moshi.models.tts import DEFAULT_DSM_TTS_REPO, TTSModel

log = logging.getLogger("kyutai")

# The voice Kris chose on 2026-10-12, and its settings (see `specs/voice.md`).
DEFAULTS = {
    "voice": "cml-tts/fr/5476_3103_000072-0001.wav",
    "padding_bonus": -3.0,
    "max_padding": 8,
    "temp": 0.6,
    "cfg_coef": 2.0,
}
MAX_TEXT = 2000


class Stopped(Exception):
    """The client went away, or the sentence is all said: the generation stops here."""


class BadRequest(Exception):
    pass


def settings_of(body: dict, valid_cfg: set[float]) -> dict:
    unknown = set(body) - set(DEFAULTS) - {"text"}
    if unknown:
        raise BadRequest(f"unknown fields: {sorted(unknown)}")
    s = {**DEFAULTS, **{k: v for k, v in body.items() if k != "text"}}
    text = body.get("text")
    if not isinstance(text, str) or not text.strip() or len(text) > MAX_TEXT:
        raise BadRequest(f"text must be a non-empty string of at most {MAX_TEXT} characters")
    s["text"] = text.strip()
    if not isinstance(s["voice"], str) or not s["voice"]:
        raise BadRequest("voice must be a voice of kyutai/tts-voices")
    try:
        s["padding_bonus"] = float(s["padding_bonus"])
        s["temp"] = float(s["temp"])
        s["cfg_coef"] = float(s["cfg_coef"])
    except (TypeError, ValueError) as e:
        raise BadRequest(str(e)) from e
    if not isinstance(s["max_padding"], int) or not 1 <= s["max_padding"] <= 64:
        raise BadRequest("max_padding must be an integer between 1 and 64")
    if not -10 <= s["padding_bonus"] <= 10:
        raise BadRequest("padding_bonus must be between -10 and 10")
    if not 0 < s["temp"] <= 2:
        raise BadRequest("temp must be in ]0, 2]")
    if s["cfg_coef"] not in valid_cfg:
        raise BadRequest(f"cfg_coef must be one of {sorted(valid_cfg)}")
    return s


class Sentence:
    """What the hooks of the generator need to know about the sentence being said."""

    def __init__(self, machine, state, padding_bonus: float):
        self.machine = machine
        self.state = state
        self.padding_bonus = padding_bonus
        self.step = 0


class Engine:
    def __init__(self):
        device = os.environ.get("KYUTAI_DEVICE", "cuda")
        dtype = torch.bfloat16 if device == "cuda" else torch.float32
        started = time.perf_counter()
        info = CheckpointInfo.from_hf_repo(DEFAULT_DSM_TTS_REPO)
        self.tts = TTSModel.from_checkpoint_info(
            info, n_q=int(os.environ.get("KYUTAI_N_Q", "32")), device=device, dtype=dtype
        )
        self.valid_cfg = self.tts.valid_cfg_conditionings
        # The 1.6B is trained with CFG distillation: the coefficient is a condition, and the
        # model runs once per step.
        assert self.valid_cfg, "a model trained with CFG distillation is expected"
        if not self.tts.multistream:
            self.tts.lm.dep_q = self.tts.n_q
        self.sample_rate = int(self.tts.mimi.sample_rate)
        self.tts.mimi.streaming_forever(1)
        self.lock = threading.Lock()
        self.voices: dict[str, object] = {}
        # One generator, kept between sentences and reset before each: building one captures its
        # CUDA graphs again, a few hundred milliseconds. The voice, the temperature and the CFG
        # coefficient are in it, so it is built again only when one of them changes.
        self.gen = None
        self.gen_key = None
        self.gen_stack = None
        self.sentence: Sentence | None = None
        log.info("model loaded in %.1f s on %s", time.perf_counter() - started, device)

    def voice_path(self, voice: str):
        if voice not in self.voices:
            try:
                self.voices[voice] = self.tts.get_voice_path(voice)
            except Exception as e:
                raise BadRequest(f"unknown voice {voice!r}: {e}") from e
        return self.voices[voice]

    def generator(self, s: dict) -> LMGen:
        key = (s["voice"], s["temp"], s["cfg_coef"])
        if self.gen_key == key:
            return self.gen
        if self.gen_stack is not None:
            self.gen_stack.close()
        tts = self.tts
        lm = tts.lm
        ids = tts.machine.token_ids
        attributes = tts.make_condition_attributes([self.voice_path(s["voice"])], cfg_coef=s["cfg_coef"])
        conditions = lm.condition_provider.prepare_and_provide([attributes])

        def on_text_logits(logits):
            if self.sentence.padding_bonus:
                logits[..., ids.pad] += self.sentence.padding_bonus

        # Each codebook is silent until its own delay has passed: one masked fill, and nothing
        # at all once the longest delay is behind.
        delays = lm.delays[lm.audio_offset:lm.audio_offset + lm.dep_q]
        silent_until = torch.tensor(delays, device=lm.device) + tts.delay_steps
        last_silent = max(delays) + tts.delay_steps

        def on_audio(tokens):
            if self.sentence.step < last_silent:
                tokens.masked_fill_((self.sentence.step < silent_until)[None, :], ids.zero)

        def on_text(tokens):
            sentence = self.sentence
            token, _ = sentence.machine.process(sentence.step, sentence.state, tokens[0].item())
            tokens[0] = token

        self.gen = LMGen(
            lm, temp=s["temp"], temp_text=s["temp"], cfg_coef=tts.cfg_coef,
            condition_tensors=conditions, on_text_logits_hook=on_text_logits,
            on_text_hook=on_text, on_audio_hook=on_audio, cfg_is_no_text=True,
        )
        self.gen_stack = self.gen.streaming(1)
        self.gen_key = key
        log.info("generator built for %s, temp %s, cfg %s", *key)
        return self.gen

    @torch.no_grad()
    def say(self, s: dict, on_pcm, stopped) -> float:
        """Synthesises the sentence, calling `on_pcm(bytes)` for each frame of sound as soon as it
        is decoded. `stopped()` is checked at every step. Returns the seconds of sound."""
        tts = self.tts
        lm = tts.lm
        gen = self.generator(s)
        gen.reset_streaming()
        tts.mimi.reset_streaming()
        machine = dataclasses.replace(tts.machine, max_padding=s["max_padding"])
        state = machine.new_state(tts.prepare_script([s["text"]], padding_between=1))
        self.sentence = sentence = Sentence(machine, state, s["padding_bonus"])
        ids = machine.token_ids
        silent = torch.full((1, lm.dep_q, 1), ids.zero, dtype=torch.long, device=lm.device)
        missing = torch.full((1, lm.n_q - lm.dep_q, 1), ids.zero, dtype=torch.long, device=lm.device)
        frames = decoded = samples = 0
        started = time.perf_counter()
        for step in range(tts.max_gen_length):
            if stopped():
                raise Stopped
            sentence.step = step
            frame = gen.step(missing, depformer_replace_tokens=silent if step < tts.delay_steps else None)
            if frame is None:
                continue
            frames += 1
            if frames <= tts.delay_steps:
                continue
            # As `simple_generate`: the first two decoded frames click, the sound ends at
            # `end_step`.
            if state.end_step is not None and decoded >= state.end_step + 2:
                break
            pcm = tts.mimi.decode(frame[:, 1:, :])
            decoded += 1
            if decoded <= 2:
                continue
            pcm = (pcm[0, 0].clamp(-1, 1) * 32767).to(torch.int16).cpu().numpy()
            if not samples:
                log.debug("first sound computed at step %d, %.0f ms (text only until step %d)",
                          step, (time.perf_counter() - started) * 1e3, tts.delay_steps)
            samples += len(pcm)
            on_pcm(pcm.astype("<i2").tobytes())
        return samples / self.sample_rate

    def wav_header(self) -> bytes:
        rate = self.sample_rate
        # Sizes unknown while streaming: the largest value, as Pocket does.
        return (b"RIFF" + struct.pack("<I", 0xFFFFFFFF) + b"WAVEfmt "
                + struct.pack("<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16)
                + b"data" + struct.pack("<I", 0xFFFFFFFF))


def closed(sock: socket.socket) -> bool:
    """True when the client has closed its side (readable, and nothing to read)."""
    try:
        readable, _, _ = select.select([sock], [], [], 0)
        return bool(readable) and sock.recv(1, socket.MSG_PEEK | socket.MSG_DONTWAIT) == b""
    except (BlockingIOError, InterruptedError):
        return False
    except OSError:
        return True


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    engine: Engine = None  # set at start

    def log_message(self, format, *args):
        log.debug(format, *args)

    def answer(self, status: int, body: dict):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/health":
            self.answer(200, {"status": "ok"})
        else:
            self.answer(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/tts":
            return self.answer(404, {"error": "not found"})
        try:
            length = int(self.headers.get("content-length", "0"))
            body = json.loads(self.rfile.read(length) or b"{}")
            if not isinstance(body, dict):
                raise BadRequest("the body must be a JSON object")
            s = settings_of(body, self.engine.valid_cfg)
        except (BadRequest, ValueError) as e:
            return self.answer(400, {"error": str(e)})
        received = time.perf_counter()
        with self.engine.lock:
            try:
                # The voice is resolved before the answer starts, so that an unknown one is a 400.
                self.engine.voice_path(s["voice"])
            except BadRequest as e:
                return self.answer(400, {"error": str(e)})
            self.send_response(200)
            self.send_header("content-type", "audio/wav")
            self.send_header("transfer-encoding", "chunked")
            self.end_headers()
            first = []

            def chunk(data: bytes):
                self.wfile.write(b"%x\r\n%s\r\n" % (len(data), data))
                self.wfile.flush()

            def sound(data: bytes):
                if not first:
                    first.append(time.perf_counter() - received)
                chunk(data)

            try:
                chunk(self.engine.wav_header())
                audio = self.engine.say(s, sound, lambda: closed(self.connection))
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()
            except (Stopped, BrokenPipeError, ConnectionResetError):
                log.info("stopped: the client went away (%.2f s in)", time.perf_counter() - received)
                self.close_connection = True
                return
            took = time.perf_counter() - received
            log.info(
                "%d chars: first sound %.0f ms, %.2f s of sound in %.2f s (rtf %.2f), vram peak %.0f MiB",
                len(s["text"]), (first[0] if first else took) * 1e3, audio, took,
                took / audio if audio else 0, torch.cuda.max_memory_allocated() / 2**20
                if torch.cuda.is_available() else 0,
            )


def main():
    logging.basicConfig(level=os.environ.get("KYUTAI_LOG", "INFO"),
                        format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    torch.set_num_threads(int(os.environ.get("KYUTAI_THREADS", "4")))
    engine = Engine()
    Handler.engine = engine
    # A first sentence, so that the first real one does not pay for the warm-up.
    engine.say(settings_of({"text": "Bonjour."}, engine.valid_cfg), lambda _: None, lambda: False)
    host = os.environ.get("KYUTAI_HOST", "127.0.0.1")
    port = int(os.environ.get("KYUTAI_PORT", "8001"))
    server = ThreadingHTTPServer((host, port), Handler)
    server.daemon_threads = True
    log.info("listening on %s:%d", host, port)
    server.serve_forever()


if __name__ == "__main__":
    main()
