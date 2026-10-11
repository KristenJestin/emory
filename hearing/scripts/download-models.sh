#!/usr/bin/env bash
# Télécharge les modèles des oreilles dans ~/.cache/emory/modeles/ (ou le dossier donné en
# argument) : Silero VAD et Parakeet TDT 0.6B v3 int8, depuis les versions officielles de
# sherpa-onnx. Ce qui est déjà là n'est pas retéléchargé.
set -euo pipefail

dossier="${1:-$HOME/.cache/emory/modeles}"
base="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models"
parakeet="sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"

mkdir -p "$dossier"

if [[ ! -f "$dossier/silero_vad.onnx" ]]; then
    echo "Silero VAD (0,6 Mo)…"
    curl --fail --location --silent --show-error \
        --output "$dossier/silero_vad.onnx.part" "$base/silero_vad.onnx"
    mv "$dossier/silero_vad.onnx.part" "$dossier/silero_vad.onnx"
fi

if [[ ! -f "$dossier/$parakeet/encoder.int8.onnx" ]]; then
    echo "Parakeet TDT 0.6B v3 int8 (480 Mo)…"
    curl --fail --location --silent --show-error \
        --output "$dossier/$parakeet.tar.bz2.part" "$base/$parakeet.tar.bz2"
    tar --extract --bzip2 --directory "$dossier" --file "$dossier/$parakeet.tar.bz2.part"
    rm "$dossier/$parakeet.tar.bz2.part"
fi

echo "Modèles dans $dossier :"
ls -1 "$dossier" "$dossier/$parakeet"
