#!/usr/bin/env bash
# Build one llama.cpp runtime pack into dist/: build-llama-runtime.sh <pack-id>
# Extra CMake flags come from $CMAKE_FLAGS (whitespace-separated).
set -euo pipefail

pack="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=runtimes/llama-cpp.env
source "$root/runtimes/llama-cpp.env"
read -ra flags <<< "${CMAKE_FLAGS:-}"

src="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/llama.cpp"
rm -rf "$src"
git clone --depth 1 --branch "$LLAMA_CPP_REF" https://github.com/ggml-org/llama.cpp "$src"

if [[ "$pack" == *rocm* ]]; then
  HIPCXX="$(hipconfig -l)/clang"
  HIP_PATH="$(hipconfig -R)"
  export HIPCXX HIP_PATH
fi

name="imperium-runtime-llama.cpp-$pack"
out="$root/dist/stage/$name"
cmake -S "$src" -B "$src/build" -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON \
  -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_TOOLS=ON \
  -DLLAMA_BUILD_SERVER=ON -DLLAMA_CURL=OFF -DCMAKE_INSTALL_PREFIX="$out" "${flags[@]}"
cmake --build "$src/build" --config Release --parallel
cmake --install "$src/build" --config Release

# CUDA packs carry the redistributable runtime libraries.
if [[ "$pack" == *cuda* ]]; then
  cuda="${CUDA_PATH:?CUDA_PATH is not set}"
  if command -v cygpath > /dev/null; then
    cuda="$(cygpath -u "$cuda")"
    dest="$out/bin"
    patterns=(-name 'cudart64_*.dll' -o -name 'cublas64_*.dll' -o -name 'cublasLt64_*.dll')
  else
    dest="$out/lib"
    patterns=(-name 'libcudart.so*' -o -name 'libcublas.so*' -o -name 'libcublasLt.so*')
  fi
  mkdir -p "$dest"
  find "$cuda" \( "${patterns[@]}" \) -exec cp -P {} "$dest/" \;
fi

cat > "$out/manifest.json" << JSON
{
  "runtime": "llama.cpp",
  "ref": "$LLAMA_CPP_REF",
  "pack": "$pack",
  "cmake_flags": "${CMAKE_FLAGS:-}"
}
JSON

tar -czf "$root/dist/$name.tar.gz" -C "$root/dist/stage" "$name"
rm -rf "$root/dist/stage"
