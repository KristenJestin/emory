# Kyutai TTS 1.6B behind serve.py: POST /tts streams a WAV, GET /health. Needs an NVIDIA card and
# the NVIDIA container runtime on the host (`--gpus all`, or `deploy.resources` in compose).
#
# The base is a plain Python image: the CUDA torch wheel brings its own CUDA libraries (cu128),
# only the driver comes from the host, through the NVIDIA runtime. A CUDA base image would add
# a second copy of the same libraries.
FROM python:3.12-slim-bookworm

RUN pip install --no-cache-dir "torch==2.9.1" --index-url https://download.pytorch.org/whl/cu128 \
 && pip install --no-cache-dir "moshi==0.2.13"

COPY serve.py /app/serve.py

# Weights (about 4 GB) and voices are downloaded on first start into HF_HOME: mount a volume there.
ENV HF_HOME=/cache/hf \
    XDG_CACHE_HOME=/cache \
    KYUTAI_HOST=0.0.0.0 \
    KYUTAI_PORT=8001 \
    KYUTAI_DEVICE=cuda \
    KYUTAI_N_Q=32 \
    KYUTAI_THREADS=4 \
    OMP_NUM_THREADS=4

EXPOSE 8001
HEALTHCHECK --interval=10s --timeout=3s --start-period=300s --retries=5 \
  CMD python -c "import os,urllib.request;urllib.request.urlopen('http://127.0.0.1:%s/health'%os.environ['KYUTAI_PORT'],timeout=2)"
CMD ["python", "/app/serve.py"]
