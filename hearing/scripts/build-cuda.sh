#!/usr/bin/env bash
# Builds `hearing` with CUDA (cargo feature `cuda`): the sherpa-onnx and onnxruntime libraries come
# from the CUDA archive of the sherpa-onnx release (the crate's own download is CPU only). The
# archive is unpacked into ~/.cache/emory/sherpa-onnx-cuda/<cuda major>/ (254 MB for CUDA 13), and
# the libraries are copied beside the binaries, so `target/release/hearing` finds them by itself.
#
#   hearing/scripts/build-cuda.sh [13|12] [extra cargo arguments...]
#
# On the machine: an NVIDIA driver and the CUDA runtime (libcudart, libcublas, libcurand), and
# cuDNN 9, of the same CUDA major as the archive (13 by default). Nothing is installed by this
# script. Run it from anywhere; the build is started with a low priority.
set -euo pipefail

major="${1:-13}"
[[ $# -gt 0 ]] && shift
version="1.13.8"
ort="1.28.2"
base="https://github.com/k2-fsa/sherpa-onnx/releases/download/v$version"
archive="sherpa-onnx-v$version-cuda-$major.x-cudnn-9.x-onnxruntime$ort-linux-x64-gpu"
folder="${EMORY_CUDA_LIBS:-$HOME/.cache/emory/sherpa-onnx-cuda}/$major"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if [[ ! -f "$folder/lib/libonnxruntime_providers_cuda.so" ]]; then
    echo "sherpa-onnx $version, CUDA $major, onnxruntime $ort…"
    mkdir -p "$folder"
    curl --fail --location --silent --show-error \
        --output "$folder/$archive.tar.bz2.part" "$base/$archive.tar.bz2"
    tar --extract --bzip2 --directory "$folder" --strip-components=1 \
        --file "$folder/$archive.tar.bz2.part" "$archive/lib"
    rm "${folder:?}/$archive.tar.bz2.part"
fi

cd "$root"
SHERPA_ONNX_LIB_DIR="$folder/lib" nice -n 19 env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
    cargo build --release -p hearing --no-default-features --features cuda "$@"
echo "Built: ${CARGO_TARGET_DIR:-$root/target}/release/hearing (set provider = \"cuda\" in its configuration)"
