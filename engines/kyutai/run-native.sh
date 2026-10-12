#!/bin/sh
# Runs the engine without Docker, on this machine's graphics card, from a Python venv that has
# `moshi` (0.2.13) and a CUDA torch:
#   python -m venv venv && venv/bin/pip install moshi==0.2.13
#   KYUTAI_VENV=venv engines/kyutai/run-native.sh
# The weights and voices go to the Hugging Face cache (~/.cache/huggingface) on first start.
set -eu
venv=${KYUTAI_VENV:?KYUTAI_VENV: the venv that has moshi}
export OMP_NUM_THREADS=${OMP_NUM_THREADS:-4}
exec nice -n "${KYUTAI_NICE:-19}" "$venv/bin/python" "$(dirname "$0")/serve.py"
