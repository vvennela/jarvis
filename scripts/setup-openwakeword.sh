#!/bin/bash
# Pinned upstream openWakeWord v0.5.1 models and local Silero endpointing.
# Native runtime has no Python dependency; Python is used only for reference tests.
set -euo pipefail
umask 077
if (($#)); then
  if [[ "$1" == --help && $# == 1 ]]; then
    printf '%s\n' 'Usage: bash scripts/setup-openwakeword.sh (Apple Silicon macOS)'
    exit 0
  fi
  printf '%s\n' 'Usage: bash scripts/setup-openwakeword.sh' >&2
  exit 2
fi
[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || {
  printf '%s\n' 'This setup prepares the Apple Silicon macOS voice model assets.' >&2
  exit 1
}
repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
[[ ! -L "$repo_dir/models" ]] || exit 1
mkdir -p "$repo_dir/models"
destination="$repo_dir/models/openwakeword"
[[ ! -L "$destination" ]] || exit 1
mkdir -p "$destination"
temporary="$(mktemp -d "$repo_dir/models/openwakeword-download.XXXXXX")"
trap 'rm -rf -- "$temporary"' EXIT
hash() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
fetch() {
  curl --fail --location --proto '=https' --proto-redir '=https' --retry 2 \
    --connect-timeout 10 --max-time 180 --silent --show-error "$1" -o "$2"
}
model() {
  local name="$1" expected="$2" path="$destination/$1"
  if [[ -e "$path" || -L "$path" ]]; then
    [[ -f "$path" && ! -L "$path" && "$(hash "$path")" == "$expected" ]] || {
      printf 'Existing model failed checksum: %s\n' "$name" >&2; exit 1;
    }
  else
    fetch "https://github.com/dscripka/openWakeWord/releases/download/v0.5.1/$name" "$temporary/$name"
    [[ "$(hash "$temporary/$name")" == "$expected" ]] || {
      printf 'Downloaded model failed checksum: %s\n' "$name" >&2; exit 1;
    }
    mv -- "$temporary/$name" "$path"
  fi
}
model melspectrogram.onnx ba2b0e0f8b7b875369a2c89cb13360ff53bac436f2895cced9f479fa65eb176f
model embedding_model.onnx 70d164290c1d095d1d4ee149bc5e00543250a7316b59f31d056cff7bd3075c1f
model hey_jarvis_v0.1.onnx 94a13cfe60075b132f6a472e7e462e8123ee70861bc3fb58434a73712ee0d2cb
# openWakeWord shares the ONNX Runtime statically linked by Sherpa. Do not
# download/load another runtime: two copies caused native process teardown faults.
# Speech endpointing remains the existing local Silero VAD, independent of wake spotting.
vad="$repo_dir/models/silero_vad.onnx"
vad_hash=9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6
if [[ -e "$vad" || -L "$vad" ]]; then
  [[ -f "$vad" && ! -L "$vad" && "$(hash "$vad")" == "$vad_hash" ]] || exit 1
else
  fetch 'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx' "$temporary/silero_vad.onnx"
  [[ "$(hash "$temporary/silero_vad.onnx")" == "$vad_hash" ]] || exit 1
  mv -- "$temporary/silero_vad.onnx" "$vad"
fi
printf 'JARVIS_VAD_MODEL_PATH=%s\n' "$vad"
printf 'JARVIS_OPENWAKEWORD_MODEL_DIR=%s\n' "$destination"
printf '%s\n' 'The upstream model is trained for Hey Jarvis; standalone Jarvis is voice-dependent.'
