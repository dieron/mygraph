#!/bin/sh
# Downloads the embedding model for semantic search (multilingual-e5-small, int8 ONNX, 135 MB)
# into a folder, at a pinned revision, and checks every file's SHA-256. The server reads it from
# MYGRAPH_MODEL_DIR (or <data_dir>/models/multilingual-e5-small); it never downloads at run time.
#
#   deploy/fetch-model.sh <data_dir>/models/multilingual-e5-small
#
# Needs curl and sha256sum (Linux, macOS with coreutils, Git Bash on Windows).
set -eu

dir=${1:?usage: fetch-model.sh <folder>}
repo=Xenova/multilingual-e5-small
rev=761b726dd34fb83930e26aab4e9ac3899aa1fa78
base=https://huggingface.co/$repo/resolve/$rev

mkdir -p "$dir"
cd "$dir"
fetch() { # <file in the repository> <name here>
  curl -fsSL --retry 3 -o "$2.part" "$base/$1"
  mv "$2.part" "$2"
}
fetch onnx/model_quantized.onnx model.onnx
fetch tokenizer.json tokenizer.json
fetch config.json config.json
fetch special_tokens_map.json special_tokens_map.json
fetch tokenizer_config.json tokenizer_config.json

sha256sum -c - <<'EOF'
f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193  model.onnx
0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39  tokenizer.json
cb99455288675345e1a4f411438d5d0adbba5fbd3a67ea4fb03c015433b996c1  config.json
d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7  special_tokens_map.json
a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b  tokenizer_config.json
EOF
