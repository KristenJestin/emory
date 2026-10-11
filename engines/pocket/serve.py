"""Starts `pocket-tts serve` with the model, voice and thread count taken from the environment.

The stock server is used as is (POST /tts streams a WAV, the model and the voice state are loaded
once at start). The only thing it cannot do is set the number of torch threads: the library pins
it to 1 when imported, so it is set here, after the import.
"""
import os

import torch
from pocket_tts import main

torch.set_num_threads(int(os.environ["POCKET_THREADS"]))
main.serve(
    host="0.0.0.0",
    port=int(os.environ["POCKET_PORT"]),
    reload=False,
    language=os.environ["POCKET_LANGUAGE"],
    config=None,
    default_voice=os.environ["POCKET_VOICE"],
    quantize=False,
)
