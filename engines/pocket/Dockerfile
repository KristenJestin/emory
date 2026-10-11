# Pocket TTS (Kyutai) behind `pocket-tts serve`: POST /tts streams a WAV, GET /health.
FROM python:3.12-slim-bookworm

# CPU-only torch (the default wheel drags in CUDA, several GB for nothing).
RUN pip install --no-cache-dir torch --index-url https://download.pytorch.org/whl/cpu \
 && pip install --no-cache-dir "pocket-tts==3.3.0"

COPY serve.py /app/serve.py

# Weights and voices are downloaded on first start into HF_HOME: mount a volume there.
ENV HF_HOME=/cache/hf \
    XDG_CACHE_HOME=/cache \
    POCKET_LANGUAGE=french \
    POCKET_VOICE=estelle \
    POCKET_THREADS=1 \
    POCKET_PORT=8000

EXPOSE 8000
HEALTHCHECK --interval=10s --timeout=3s --start-period=120s --retries=5 \
  CMD python -c "import os,urllib.request;urllib.request.urlopen('http://127.0.0.1:%s/health'%os.environ['POCKET_PORT'],timeout=2)"
CMD ["python", "/app/serve.py"]
