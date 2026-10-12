# Pinyin phrase data

`phrases.tsv` is derived from **all 2,552 entries** in `dict2.ts` through
`dict5.ts` in [pinyin-pro](https://github.com/zh-lx/pinyin-pro), commit
`0785234b4dc0fa3ea9f244e3d77703b2df6a9db4`.

Original paths: `packages/pinyin-pro/lib/data/dict{2,3,4,5}.ts`.
Copyright (c) 2022-present zh-lx, MIT; the original LICENSE is included here.
No corpus-specific entries were added or removed.

Transformation: extract each CJK object key and its space-separated reading;
NFD-decompose each syllable, retain ASCII a–z, then emit tab-separated
`phrase`, `concatenated syllables`, `first letter of each syllable`, sorted by
phrase. Every entry has exactly one syllable per character. Runtime lookup
uses the longest available 2–5 character phrase and falls back to the system
single-character reading. The table is not a complete Chinese dictionary;
unlisted words and personal names can still have ambiguous readings.
