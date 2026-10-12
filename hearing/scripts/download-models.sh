#!/usr/bin/env bash
# Downloads the hearing models into ~/.cache/emory/models/ (or the folder given as argument):
# Silero VAD and Parakeet TDT 0.6B v3 int8, from the official sherpa-onnx releases. What is
# already there is not downloaded again.
#
# It also writes `bpe.vocab` into the Parakeet folder, which the hotwords (the vocabulary of
# Kris's names) need and the model does not ship: the same tokens as `tokens.txt`, each with the
# score -id, in the `token<TAB>score` format sherpa-onnx reads. Made from the model's own tokens,
# it cannot disagree with it.
#
# With EMORY_FP32=1 it also downloads the full-precision Parakeet (2.5 GB, from Hugging Face), the
# one a GPU wants (the int8 model gains nothing there): see hearing/README.md.
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

if [[ ! -f "$folder/$parakeet/bpe.vocab" ]]; then
    echo "bpe.vocab for the hotwords…"
    awk '{ printf "%s\t%d\n", $1, -$2 }' "$folder/$parakeet/tokens.txt" \
        > "$folder/$parakeet/bpe.vocab.part"
    mv "$folder/$parakeet/bpe.vocab.part" "$folder/$parakeet/bpe.vocab"
fi

if [[ "${EMORY_FP32:-0}" == "1" ]]; then
    full="sherpa-onnx-nemo-parakeet-tdt-0.6b-v3"
    mkdir -p "$folder/$full"
    for file in decoder.onnx encoder.onnx encoder.weights joiner.onnx tokens.txt; do
        if [[ ! -s "$folder/$full/$file" ]]; then
            echo "Parakeet fp32: $file…"
            curl --fail --location --silent --show-error \
                --output "$folder/$full/$file.part" \
                "https://huggingface.co/csukuangfj/$full/resolve/main/$file"
            mv "$folder/$full/$file.part" "$folder/$full/$file"
        fi
    done
    awk '{ printf "%s\t%d\n", $1, -$2 }' "$folder/$full/tokens.txt" > "$folder/$full/bpe.vocab"
fi

echo "Models in $folder:"
ls -1 "$folder" "$folder/$parakeet"
