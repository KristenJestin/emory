#!/usr/bin/env bash
# Downloads the hearing models into ~/.cache/emory/models/ (or the folder given as argument):
# Silero VAD and Parakeet TDT 0.6B v3 int8, from the official sherpa-onnx releases. What is
# already there is not downloaded again.
set -euo pipefail

folder="${1:-$HOME/.cache/emory/models}"
base="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models"
parakeet="sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"

mkdir -p "$folder"

if [[ ! -f "$folder/silero_vad.onnx" ]]; then
    echo "Silero VAD (0.6 MB)…"
    curl --fail --location --silent --show-error \
        --output "$folder/silero_vad.onnx.part" "$base/silero_vad.onnx"
    mv "$folder/silero_vad.onnx.part" "$folder/silero_vad.onnx"
fi

if [[ ! -f "$folder/$parakeet/encoder.int8.onnx" ]]; then
    echo "Parakeet TDT 0.6B v3 int8 (480 MB)…"
    curl --fail --location --silent --show-error \
        --output "$folder/$parakeet.tar.bz2.part" "$base/$parakeet.tar.bz2"
    tar --extract --bzip2 --directory "$folder" --file "$folder/$parakeet.tar.bz2.part"
    rm "${folder:?}/${parakeet:?}.tar.bz2.part"
fi

echo "Models in $folder:"
ls -1 "$folder" "$folder/$parakeet"
