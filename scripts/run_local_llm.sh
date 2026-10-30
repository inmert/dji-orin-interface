#!/usr/bin/env bash
# Start llama.cpp's llama-server on the Orin's GPU as the local understanding
# model (apps/server/src/understand/local.rs). Runs as the user, not root;
# the web server reaches it at LOCAL_LLM_URL (default http://127.0.0.1:8081).
#
#   scripts/run_local_llm.sh                      # Llama 3.2 3B Instruct Q4_K_M
#   LLM_GGUF=~/models/Llama-3.2-1B-Instruct-Q4_K_M.gguf scripts/run_local_llm.sh
#
# LLAMA_SERVER   the binary (default ~/tools/llama.cpp/build/bin/llama-server,
#                built with -DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=87)
# LLM_GGUF       the model file (default ~/models/Llama-3.2-3B-Instruct-Q4_K_M.gguf)
# LLM_PORT       the port (default 8081)
# LLM_CTX        context length (default 2048: the instructions, a sentence
#                and the plan fit in a few hundred tokens)
#
# Memory: the Orin's 8 GB is shared with the GPU. The 3B Q4_K_M model takes
# about 2.3 GB resident with this context; the 1B one about 1 GB.
set -euo pipefail

LLAMA_SERVER="${LLAMA_SERVER:-$HOME/tools/llama.cpp/build/bin/llama-server}"
LLM_GGUF="${LLM_GGUF:-$HOME/models/Llama-3.2-3B-Instruct-Q4_K_M.gguf}"
LLM_PORT="${LLM_PORT:-8081}"
LLM_CTX="${LLM_CTX:-2048}"

[ -x "$LLAMA_SERVER" ] || { echo "no llama-server at $LLAMA_SERVER" >&2; exit 1; }
[ -s "$LLM_GGUF" ] || { echo "no model at $LLM_GGUF" >&2; exit 1; }

# -ngl 99: every layer on the GPU. --jinja: the model's own chat template.
# One slot, one request at a time: the server asks for one plan at a time.
exec "$LLAMA_SERVER" \
  --model "$LLM_GGUF" \
  --host 127.0.0.1 --port "$LLM_PORT" \
  --ctx-size "$LLM_CTX" --n-gpu-layers 99 --flash-attn on \
  --parallel 1 --jinja --no-webui --log-timestamps \
  "$@"
