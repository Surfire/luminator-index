# Luminator

A local search engine for filenames, document text, and meaning.

Luminator 计划以 Rust 管理本地索引和查询，结合原生 macOS 能力，向 Tinycast、Raycast 扩展和命令行提供同一套搜索引擎。仓库名为 `luminator-index`。

**当前状态：文件名引擎、CLI 和可选中文全文已实现。** 支持中文、全拼/首字母、路径、筛选、SQLite 持久化、FSEvents 实时更新及离线事件恢复。Tinycast 的独立本地 AGPL 体验分支已接入主搜索；可选正文支持已下载的 TXT/MD、PDF 文本层和 DOCX；模型权重已校验，尚未接入推理或 Raycast 扩展。

## 编译与试用

需要 macOS 14+、Rust stable 和 Xcode Command Line Tools；已在 Apple Silicon 实测。使用系统 SQLite / CoreFoundation / FSEvents，不安装后台服务。Cargo 依赖固定在 `Cargo.lock`；仓库默认使用国内 rsproxy 镜像，可按自己的网络配置修改 `.cargo/config.toml`。

```sh
cargo build --release --locked

# 显式选择一个目录；数据库放在被索引目录之外。
target/release/luminator --db /tmp/luminator.sqlite scan /path/to/library
target/release/luminator --db /tmp/luminator.sqlite query '健康'
target/release/luminator --db /tmp/luminator.sqlite query 'jiankang'
target/release/luminator --db /tmp/luminator.sqlite query 'jk' --extension pdf

# 前台运行实时更新；Ctrl+C 停止，不注册登录项。
target/release/luminator --db /tmp/luminator.sqlite serve --socket /tmp/luminator.sock

# 在另一终端查询同一个常驻索引，避免每次加载数据库。
target/release/luminator query '健康' --socket /tmp/luminator.sock
target/release/luminator status --socket /tmp/luminator.sock
```

输出为 JSON。每个数据库对应一个根目录；多个目录目前分别建库。直接查询数据库读取保存的快照；运行 `serve` 才持续更新，也可以再次运行 `scan` 刷新。每份数据库同一时间只允许一个 scan/serve 写入者。socket 路径也应位于被索引目录之外。

根目录本身也参与中文、全拼、首字母和路径搜索。早期 v2 索引把根目录存为空名称；升级程序后先停止其 `serve`，对同一目录再次执行 `scan` 即可补齐，无需读取正文或新建数据库。`status.entries` 仍只统计根目录里的条目，不计根目录本身。

查询使用不区分大小写的 NFC 规范化子串；包含 `/` 时查完整路径，返回原始文件路径。支持 `--kind file|directory|symlink`、`--extension pdf`、`--under /absolute/path` 和 `--limit 20`。拼音使用通用 MIT 词组表解决常见多音字，未收录的词回退到系统单字读音；不处理声调。默认 `--mode auto` 融合名称与拼音，优先精确名称，降低仅扩展名命中的排名；`--mode pinyin` 强调中文全拼/首字母，`--mode filename` 只查字面名称。两字母缩写存在同音/同首字母歧义，建议使用更长缩写、全拼或目录筛选。未实现 AND/OR/正则查询语法。

2026-10-13 的优化将索引格式更新为 **v2**。旧格式库会被明确拒绝并保持原样；请用另一个数据库路径重新 `scan`，无需读取正文。百万索引的名称匹配最多使用 4 个工作线程，小索引继续单线程；后台 socket 等待可立即被查询唤醒。

名称模式只读名称元数据，不读文件正文，不跟随符号链接；遇到不可枚举的云占位目录会保留目录名称并统计缺口，不向下遍历。已在一个获授权的真实 OneDrive 目录验证可枚举名称、拼音和增量；这不保证云端所有目录均能枚举。没有 Spotlight 补充索引。请从一个明确选定的小目录开始体验。

## 阶段 1 验证

实测使用 **100 万个实际磁盘文件/目录**，包含约 20% 中文、5% NFD 拉丁文件名。Apple M5 Max / 128 GB 上，2026-10-13 复测普通查询 p95 **4.24 ms**，百万索引 socket 往返 p95 **3.39 ms**，单文件增量 p95 **20–34 ms**，服务物理内存峰值约 **110 MiB**。口径、并发负载下的结果和边界见 [优化报告](docs/filename-optimization.md)，原始统计保存在 `benchmarks/results/`。这些是本机合成数据结果，不代表所有 Mac、云目录或未来 UI 的表现；[阶段 1 报告](docs/phase1-results.md) 保留优化前记录。

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings

# 独立合成数据；此命令会创建百万条空文件/目录，拒绝已有目录。
mkdir -p benchmarks/local-results
target/release/luminator fixture benchmarks/local-results/million-tree --entries 1000000
target/release/luminator --db benchmarks/local-results/million.sqlite scan benchmarks/local-results/million-tree
target/release/luminator --db benchmarks/local-results/million.sqlite benchmark --queries benchmarks/queries.json --repeats 20
uv run --no-project python3 benchmarks/live.py --db benchmarks/local-results/million.sqlite --root benchmarks/local-results/million-tree --output benchmarks/local-results/live.json
```

后台写入线程负责 FSEvents、扫描和 SQLite 事务，查询继续使用上一份已提交索引。两者共享不可变数据块，仅修改时复制相关块；事务提交后发布共享快照，避免整表复制。名称/拼音使用相邻字节签名跳过不可能匹配的记录，仍由原有匹配器验证和排序；连续输入复用完整候选集，不从 Top 20 裁剪候选。百万条目本轮首段普通查询 socket p95 **2.12 ms**、连续输入 **1.34 ms**、批量更新期间 **1.83 ms**，峰值约 **122–132 MiB**。详见 [共享索引与首段查询实测](docs/shared-index-search.md)，上一轮记录见 [交互与批量更新优化](docs/responsive-search.md)。

高频长期重命名/删除会留下旧名称池和 tombstone，尚未实现在线压缩；重新启动会重建紧凑名称池。名称元数据修改时间以秒记录；独立正文索引用纳秒 mtime/ctime 与文件身份识别内容变更。

正文命令、隐私边界、开关及真实测试见 [中文全文搜索](docs/content-search.md)。后续语义推理见 [EmbeddingGemma 2 接入方案](docs/semantic-next-stage.md)。

## 完整 v0.1 的后续目标

- 在配置的目录中快速搜索文件名、路径、中文和拼音。
- 搜索已下载到本地的 PDF 文本、DOCX、TXT、Markdown 内容。
- 使用本地 EmbeddingGemma 2 做语义检索，返回原文片段和文件位置。
- Tinycast 主搜索框直接显示结果，无需先输入命令名；文件名、全文、语义能力可分别开关。
- Raycast 扩展调用同一引擎，提供搜索命令；不承诺向 Raycast 主搜索框注入文件结果。

## 方案入口

| 文档 | 内容 |
| --- | --- |
| [阶段 1 实测](docs/phase1-results.md) | 已实现能力、百万条目性能、复现命令及未验收范围 |
| [名称优化与 Spotlight 对照](docs/filename-optimization.md) | 多音字、排序、百万条目复测与同范围文件名对照；私人原始数据只保存在本机 |
| [技术方案与第一版范围](docs/technical-plan.md) | 完整 v0.1 目标、Tinycast 修改清单、Raycast 兼容和后续实施顺序 |
| [源码复用与许可证](docs/reuse-and-licensing.md) | 固定上游版本、直接复用、Rust 重写、必须调整的代码 |
| [性能与效果验收](docs/performance-acceptance.md) | 数据集、时间口径、目标、正确性和质量验收 |

## 实现原则

文件名先返回，内容和语义随后补充；模型不阻塞普通搜索。高性能扫描和推理运行时优先复用已有代码，Rust 负责统一生命周期、增量更新、查询和客户端接口。第一版面向 Apple Silicon / macOS 14+；Tinycast 适配遵循 Tinycast 自身 macOS 26+ 的要求。

源码参考于 2026-10-12 核对：[Oil Find](https://github.com/oil-oil/oil-find)、[DigUp](https://github.com/ARahim3/DigUp)、[Tinycast](https://github.com/abue-ammar/tinycast)。具体提交与复用边界见文档。名称是工作名，可在首次发布前调整。

## 开源方式

本仓库原创内容采用 [MIT](LICENSE)。已复用的 Oil Find C 扫描/匹配源码保持原样及原 MIT 版权；通用拼音词组数据派生自 MIT 的 pinyin-pro，见 [第三方说明](THIRD_PARTY_NOTICES.md)。Tinycast 的未来修改放在其单独的 AGPL 仓库中。模型权重与本机索引不提交到本仓库。

公开仓库：[Surfire/luminator-index](https://github.com/Surfire/luminator-index)。
