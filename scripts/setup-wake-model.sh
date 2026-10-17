#!/bin/bash
# Install the pinned local streaming Jarvis keyword model.
set -euo pipefail
if (($#)); then
  if [[ "$1" == --help && $# == 1 ]]; then
    printf '%s\n' 'Usage: bash scripts/setup-wake-model.sh'
    exit 0
  fi
  printf '%s\n' 'Usage: bash scripts/setup-wake-model.sh' >&2
  exit 2
fi
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
name=sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01
destination="$repo_dir/models/$name"
if [[ -e "$destination" ]]; then
  [[ -d "$destination" && ! -L "$destination" ]] || exit 1
  for file in encoder-epoch-12-avg-2-chunk-16-left-64.int8.onnx decoder-epoch-12-avg-2-chunk-16-left-64.onnx joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx tokens.txt; do
    [[ -f "$destination/$file" && ! -L "$destination/$file" ]] || exit 1
  done
else
  mkdir -p "$repo_dir/models"
  temporary="$(mktemp -d "$repo_dir/models/download.XXXXXX")"
  trap 'rm -rf -- "$temporary"' EXIT
  curl --fail --location --retry 2 --connect-timeout 10 --max-time 180 \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/$name.tar.bz2" \
    -o "$temporary/model.tar.bz2"
  expected=f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a
  actual="$(shasum -a 256 "$temporary/model.tar.bz2" | cut -d ' ' -f 1)"
  if [[ "$actual" != "$expected" ]]; then
    printf '%s\n' 'Keyword model checksum mismatch.' >&2
    exit 1
  fi
  tar -xjf "$temporary/model.tar.bz2" -C "$temporary"
  mv "$temporary/$name" "$destination"
fi
printf 'JARVIS_KWS_MODEL_DIR=%s\n' "$destination"
