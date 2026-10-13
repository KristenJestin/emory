#!/usr/bin/env bash
# Measures Parakeet and Silero on recorded sentences, CPU against GPU, one setting per process:
#
#   int8 CPU, fp32 CPU, fp32 CUDA, each greedy and beam (+ the user's vocabulary),
#   and Silero on the CPU against Silero on CUDA.
#
# For each run: the load and warm-up times, the time per sentence and per round, the CPU used
# during the transcription (cores), the VRAM of the process (nvidia-smi, sampled every 0.2 s),
# and how many sentences have other words than the CPU int8 greedy run. The result is a Markdown
# table, `results.md`, beside the raw files of each run (`*.json`, `*.texts`, `*.vram`, `*.err`).
#
#   hearing/scripts/measure-gpu.sh --owner-ok [options] file.wav [file.wav ...]
#
#   --owner-ok          required for anything that uses the GPU: the owner has agreed, stopped
#                       whatever else holds the card (voxtype on the development PC) and left the
#                       machine to this script.
#   --cpu-only          only the CPU runs; no nvidia-smi, no GPU, no --owner-ok needed.
#   --dry-run           prints what would run, runs nothing (no GPU query either).
#   --rounds N          rounds over the sentences per run (default 5; the best round is the
#                       measure that holds on a loaded machine).
#   --hotwords FILE     the vocabulary for the beam runs (default: hotwords.txt of the data folder
#                       if it exists, else the beam runs have no words).
#   --out DIR           where the results go (default ~/.local/state/emory/measure-gpu/<date>).
#   --min-free MiB      free VRAM required before the GPU runs (default 4500).
#
# The files must be 16 kHz mono 16-bit (ffmpeg -i in.wav -ar 16000 -ac 1 -c:a pcm_s16le out.wav).
# They are read in place, never copied, never sent anywhere; the results folder holds the texts
# of the recordings (`*.texts`): it is private, keep it out of the repository.
#
# Needs the CUDA build (`hearing/scripts/build-cuda.sh`, which also builds `hearing-measure`) and
# the models (`EMORY_FP32=1 hearing/scripts/download-models.sh`). Runs are sequential, at the
# lowest CPU priority, 4 threads each. Environment: EMORY_MODELS (default ~/.cache/emory/models),
# CARGO_TARGET_DIR (where the build is), EMORY_MEASURE_BIN (the hearing-measure binary itself).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
models="${EMORY_MODELS:-$HOME/.cache/emory/models}"
bin="${EMORY_MEASURE_BIN:-${CARGO_TARGET_DIR:-$root/target}/release/hearing-measure}"
data="${XDG_DATA_HOME:-$HOME/.local/share}/emory"

owner_ok=0 cpu_only=0 dry=0 rounds=5 min_free=4500 hotwords="" out=""
files=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --owner-ok) owner_ok=1 ;;
        --cpu-only) cpu_only=1 ;;
        --dry-run) dry=1 ;;
        --rounds) rounds="$2"; shift ;;
        --hotwords) hotwords="$2"; shift ;;
        --out) out="$2"; shift ;;
        --min-free) min_free="$2"; shift ;;
        -h|--help) sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d;s/^# \{0,1\}//'; exit 0 ;;
        -*) echo "unknown option: $1" >&2; exit 2 ;;
        *) files+=("$1") ;;
    esac
    shift
done
[[ ${#files[@]} -gt 0 ]] || { echo "give at least one 16 kHz mono wav file (--help)" >&2; exit 2; }
out="${out:-${XDG_STATE_HOME:-$HOME/.local/state}/emory/measure-gpu/$(date +%Y%m%d-%H%M%S)}"
if [[ -z "$hotwords" && -s "$data/hotwords.txt" ]]; then
    hotwords="$data/hotwords.txt"
fi

int8="$models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
fp32="$models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3"

# The matrix: provider, precision, decoding, Silero's provider. The first GPU run comes first, so
# a card or a library that does not work stops everything within a minute, not after the CPU runs.
cpu_runs=("cpu int8 greedy cpu" "cpu int8 beam cpu" "cpu fp32 greedy cpu" "cpu fp32 beam cpu")
gpu_runs=("cuda fp32 greedy cpu" "cuda fp32 beam cpu")
vad_run="cuda fp32 greedy cuda"   # Silero on the card, once, on the first file

if ((dry == 0)); then
    [[ -x "$bin" ]] || { echo "$bin is missing: hearing/scripts/build-cuda.sh" >&2; exit 1; }
    for dir in "$int8" "$fp32"; do
        [[ -d "$dir" ]] || { echo "$dir is missing: EMORY_FP32=1 hearing/scripts/download-models.sh" >&2; exit 1; }
    done
    [[ -f "$fp32/encoder.weights" ]] || { echo "the fp32 model is incomplete ($fp32/encoder.weights)" >&2; exit 1; }
    for file in "${files[@]}"; do
        [[ -f "$file" ]] || { echo "$file: not a file" >&2; exit 1; }
    done
fi

if ((cpu_only == 0)); then
    if ((owner_ok == 0 && dry == 0)); then
        echo "this uses the GPU: pass --owner-ok once the owner has agreed and freed the card (--help)," >&2
        echo "or --cpu-only / --dry-run" >&2
        exit 2
    fi
    if ((dry == 0)); then
        [[ -f "$(dirname "$bin")/libonnxruntime_providers_cuda.so" ]] || {
            echo "$(dirname "$bin") holds no CUDA libraries: this is a CPU build, hearing/scripts/build-cuda.sh" >&2
            exit 1
        }
        command -v nvidia-smi > /dev/null || { echo "nvidia-smi not found: no NVIDIA driver here" >&2; exit 1; }
        if pgrep -x voxtype > /dev/null; then
            echo "voxtype is running and holds the card: the owner stops it first. Nothing was started." >&2
            exit 1
        fi
        free=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits | head -1 | tr -d ' ')
        if ((free < min_free)); then
            echo "only $free MiB of VRAM are free ($min_free needed): something else holds the card." >&2
            nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv >&2 || true
            exit 1
        fi
    fi
fi

if ((dry == 0)); then
    mkdir -p "$out"
    {
        echo "date: $(date -Is)"
        echo "load average at start: $(cut -d' ' -f1-3 /proc/loadavg)"
        echo "files: ${files[*]}"
        echo "rounds: $rounds   hotwords: ${hotwords:-none}"
        ((cpu_only == 0)) && nvidia-smi --query-gpu=name,driver_version,memory.total,memory.used \
            --format=csv,noheader
        ((cpu_only == 0)) && nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader
    } > "$out/context.txt" || true
fi

# run_one <wav> <provider> <precision> <decoding> <vad provider>
run_one() {
    local file="$1" provider="$2" precision="$3" decoding="$4" vad="$5"
    local stem tag dir
    stem="$(basename "${file%.*}")"
    tag="$stem.$provider-$precision-$decoding"
    [[ "$vad" == cuda ]] && tag="$tag-vadcuda"
    dir="$int8"; [[ "$precision" == fp32 ]] && dir="$fp32"
    local cmd=(nice -n 19 env OMP_NUM_THREADS=4 "$bin" run
        --parakeet "$dir" --provider "$provider" --precision "$precision"
        --decoding "$decoding" --vad-provider "$vad" --rounds "$rounds"
        --tag "$tag" --texts "$out/$tag.texts")
    [[ "$decoding" == beam && -n "$hotwords" ]] && cmd+=(--hotwords "$hotwords")
    cmd+=("$file")
    if ((dry)); then
        echo "${cmd[*]} > $out/$tag.json"
        return
    fi
    echo "[$(date +%T)] $tag"
    if ((cpu_only)); then
        "${cmd[@]}" > "$out/$tag.json" 2> "$out/$tag.err" || fail "$tag"
    else
        # The card as the others leave it, then the process's own use and the card's, every 0.2 s.
        echo "0 $(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | head -1 | tr -d ' ')" > "$out/$tag.vram"
        "${cmd[@]}" > "$out/$tag.json" 2> "$out/$tag.err" &
        local pid=$! status=0
        while kill -0 "$pid" 2> /dev/null; do
            local mine total
            mine=$(nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits \
                | awk -F', ' -v p="$pid" '$1 == p { print $2 }')
            total=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | head -1 | tr -d ' ')
            echo "${mine:-0} $total" >> "$out/$tag.vram"
            sleep 0.2
        done
        wait "$pid" || status=$?
        ((status == 0)) || fail "$tag"
    fi
    sleep 3   # the card frees the memory of the process before the next one
}

fail() {
    echo "the run $1 failed (details in $out/$1.err):" >&2
    tail -5 "$out/$1.err" >&2 || true
    exit 1
}

if ((cpu_only)); then
    runs=("${cpu_runs[@]}")
else
    runs=("${gpu_runs[0]}" "${cpu_runs[@]}" "${gpu_runs[1]}")
fi
first=1
for file in "${files[@]}"; do
    for run in "${runs[@]}"; do
        # shellcheck disable=SC2086
        run_one "$file" $run
    done
    if ((cpu_only == 0 && first)); then
        # shellcheck disable=SC2086
        run_one "$file" $vad_run
        first=0
    fi
done

if ((dry == 0)); then
    "$bin" table "$out" > "$out/results.md"
    cat "$out/results.md"
    echo "Results in $out"
fi
