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
