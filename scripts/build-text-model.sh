#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
source_dir=.cache/llama.cpp
build_dir=.cache/llama-build
if [[ ! -d "$source_dir" ]]; then
 git clone --quiet --depth 1 --branch b11461 https://github.com/ggml-org/llama.cpp "$source_dir"
fi
[[ "$(git -C "$source_dir" rev-parse HEAD)" == 4d756bc72bf00a4aacf410ae15a2d315f3db400d ]]
if git -C "$source_dir" apply --check ../../vendor/digup/llama.cpp-no-logits.patch 2>/dev/null; then
 git -C "$source_dir" apply ../../vendor/digup/llama.cpp-no-logits.patch
else
 git -C "$source_dir" apply --reverse --check ../../vendor/digup/llama.cpp-no-logits.patch
fi
cmake -S "$source_dir" -B "$build_dir" -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
 -DGGML_METAL=ON -DGGML_METAL_EMBED_LIBRARY=ON -DGGML_NATIVE=OFF \
 -DLLAMA_BUILD_MTMD=OFF -DLLAMA_BUILD_COMMON=OFF -DLLAMA_BUILD_TOOLS=OFF \
 -DLLAMA_BUILD_SERVER=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_TESTS=OFF \
 -DLLAMA_BUILD_APP=OFF -DLLAMA_BUILD_UI=OFF -DLLAMA_OPENSSL=OFF
cmake --build "$build_dir" --config Release -j 4 --target llama
mkdir -p .cache/CLlama target/release
xcrun libtool -static -no_warning_for_no_symbols -o .cache/CLlama/libllamacpp.a \
 "$build_dir/src/libllama.a" "$build_dir/ggml/src/libggml.a" "$build_dir/ggml/src/libggml-base.a" \
 "$build_dir/ggml/src/libggml-cpu.a" "$build_dir/ggml/src/ggml-metal/libggml-metal.a" \
 "$build_dir/ggml/src/ggml-blas/libggml-blas.a"
cp "$source_dir/include/llama.h" "$source_dir"/ggml/include/*.h .cache/CLlama/
cat > .cache/CLlama/module.modulemap <<'MODULE'
module CLlama [system] {
 header "llama.h"
 link "llamacpp"
 link "c++"
 link framework "Accelerate"
 link framework "Foundation"
 link framework "Metal"
 link framework "MetalKit"
 export *
}
MODULE
swiftc -O -parse-as-library -I .cache/CLlama -L .cache/CLlama native/TextEmbedder.swift -o target/release/LuminatorTextEmbedder
codesign --force --sign - target/release/LuminatorTextEmbedder
