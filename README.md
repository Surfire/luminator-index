# Luminator

A local search engine for filenames, document text, and meaning.

Luminator 计划以 Rust 管理本地索引和查询，结合原生 macOS 能力，向 Tinycast、Raycast 扩展和命令行提供同一套搜索引擎。仓库名为 `luminator-index`。

**当前状态：设计阶段。只有技术方案和 MIT 协议，没有可运行的引擎、插件或性能测试结果。**

## 第一版想交付什么

- 在配置的目录中快速搜索文件名、路径、中文和拼音。
- 搜索已下载到本地的 PDF 文本、DOCX、TXT、Markdown 内容。
- 使用本地 EmbeddingGemma 2 做语义检索，返回原文片段和文件位置。
- Tinycast 主搜索框直接显示结果，无需先输入命令名；文件名、全文、语义能力可分别开关。
- Raycast 扩展调用同一引擎，提供搜索命令；不承诺向 Raycast 主搜索框注入文件结果。

## 方案入口

| 文档 | 内容 |
| --- | --- |
| [技术方案与第一版范围](docs/technical-plan.md) | 架构、索引、调度、Tinycast 修改清单、Raycast 兼容和实施顺序 |
| [源码复用与许可证](docs/reuse-and-licensing.md) | 固定上游版本、直接复用、Rust 重写、必须调整的代码 |
| [性能与效果验收](docs/performance-acceptance.md) | 数据集、时间口径、目标、正确性和质量验收 |

## 实现原则

文件名先返回，内容和语义随后补充；模型不阻塞普通搜索。高性能扫描和推理运行时优先复用已有代码，Rust 负责统一生命周期、增量更新、查询和客户端接口。第一版面向 Apple Silicon / macOS 14+；Tinycast 适配遵循 Tinycast 自身 macOS 26+ 的要求。

源码参考于 2026-10-12 核对：[Oil Find](https://github.com/oil-oil/oil-find)、[DigUp](https://github.com/ARahim3/DigUp)、[Tinycast](https://github.com/abue-ammar/tinycast)。具体提交与复用边界见文档。名称是工作名，可在首次发布前调整。

## 开源方式

本仓库原创内容采用 [MIT](LICENSE)。未来引入的第三方代码保留原作者版权和许可证；Tinycast 的修改放在其单独的 AGPL 仓库中。模型权重与本机索引不提交到本仓库。

公开仓库：[Surfire/luminator-index](https://github.com/Surfire/luminator-index)。当前公开内容为设计文档；实现与性能结果尚未发布。
