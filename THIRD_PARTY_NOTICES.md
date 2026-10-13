# Third-party notices

`vendor/oil-find/coilfind.c`, `coilfind.h`, and `LICENSE` are copied without
modification from [Oil Find](https://github.com/oil-oil/oil-find) commit
`36559e11812d65df01ffd0de132ba1a9a278701d`:

- Original paths: `Sources/COilFind/coilfind.c`, `Sources/COilFind/include/coilfind.h`.
- Copyright (c) 2026 Lin Zhihuang. MIT license, reproduced in `vendor/oil-find/LICENSE`.
- Used for bulk directory enumeration and SIMD matching. No Pro or Tinycast code is included.

The CoreFoundation pinyin table approach in `src/native.rs` follows
`Sources/OilFindCore/Pinyin.swift` at the same commit. The implementation uses
Apple's system transliterator; single-character readings do not resolve contextual polyphones.

`vendor/pinyin-pro/phrases.tsv` is derived from all phrase entries in pinyin-pro
`dict2.ts`–`dict5.ts`, commit `0785234b4dc0fa3ea9f244e3d77703b2df6a9db4`.
Copyright (c) 2022-present zh-lx, MIT. The original license and transformation
details are in `vendor/pinyin-pro/`. The phrase table overrides single-character
defaults; it does not use private corpus filenames or model inference.

Rust dependency versions are pinned in `Cargo.lock`; their licenses remain
their own. SQLite is linked from macOS; CoreFoundation, CoreServices and
Accelerate are Apple system APIs (Accelerate is not used in phase 1).

`native/ContentExtractor.swift` adapts the PDFKit text-layer and Cocoa DOCX
extraction methods in DigUp `Sources/DigUpKit/Extractors.swift`, commit
`6faa29e4c315a3a591de1b41bd78751bdf39588d`:

- Source: https://github.com/ARahim3/DigUp
- Copyright (c) 2026 Abdur Rahim. MIT; full license in `vendor/digup/LICENSE`.
- Changes: standalone JSON helper, kernel no-materialization policy, regular-file
  checks, bounded input/output, paragraph/page locators; no OCR or media models.
- `docs/embeddinggemma-2-manifest.json` records the text GGUF revision/size/hash from
  DigUp's `ModelFiles.swift`. Weights are downloaded separately, never bundled
  with this MIT repository. The optional text inference experiment below reuses its runtime setup.

`native/TextEmbedder.swift`, `scripts/build-text-model.sh` and
`vendor/digup/llama.cpp-no-logits.patch` adapt DigUp's `LlamaEmbedder.swift`,
`build-llama.sh`, and the identically named patch at the same commit above.
Changes: text-only JSONL worker, 2048-token bound, explicit rejection of oversized
input, no HTTP server or app lifetime coupling. DigUp copyright/license retained.

The optional worker links llama.cpp b11461, commit
`4d756bc72bf00a4aacf410ae15a2d315f3db400d` (MIT, Georgi Gerganov and contributors).
The build checks this commit and includes DigUp's no-logits embedding patch.
Full llama.cpp license: `vendor/llama.cpp/LICENSE`. The runtime and model weights
are not automatically installed with the filename CLI or Tinycast preview.
