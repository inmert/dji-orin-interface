#!/bin/bash
#
# Build whisper.cpp with CUDA for the server's voice transcription, and fetch
# the English models. Clones into $WHISPER_DIR (default ~/repos/third_party/
# whisper.cpp); the server finds the binary and model there by default
# (WHISPER_CLI / WHISPER_MODEL override). Takes 10–20 minutes on an Orin Nano.
#
# Usage: scripts/setup_whisper.sh [model …]      (default: base.en small.en)
# Run it as yourself, not with sudo: run_server.sh looks in your home.
set -euo pipefail

WHISPER_DIR="${WHISPER_DIR:-$HOME/repos/third_party/whisper.cpp}"
MODELS=("${@:-base.en}")
[ $# -gt 0 ] || MODELS=(base.en small.en)
export PATH=/usr/local/cuda/bin:$PATH

if [ ! -d "$WHISPER_DIR/.git" ]; then
    mkdir -p "$(dirname "$WHISPER_DIR")"
    git clone --depth 1 https://github.com/ggml-org/whisper.cpp.git "$WHISPER_DIR"
fi
cd "$WHISPER_DIR"
cmake -B build -DGGML_CUDA=1 -DCMAKE_BUILD_TYPE=Release -DWHISPER_BUILD_TESTS=OFF
cmake --build build -j"$(nproc)" --config Release --target whisper-cli
for m in "${MODELS[@]}"; do
    [ -f "models/ggml-$m.bin" ] || sh models/download-ggml-model.sh "$m"
done
echo "OK: $WHISPER_DIR/build/bin/whisper-cli, models: ${MODELS[*]}"
