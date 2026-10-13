#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/release
swiftc -O -parse-as-library native/ContentExtractor.swift -o target/release/LuminatorContentExtractor
codesign --force --sign - target/release/LuminatorContentExtractor
