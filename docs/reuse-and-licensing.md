# 源码复用与许可证边界

核对日期：2026-10-12。三个本地 checkout 干净，核对时 origin HEAD 与以下提交一致。本表是实现计划；本仓库当前未复制第三方实现代码。

## 1. 固定源码版本

| 项目 | 提交 | 许可证 |
| --- | --- | --- |
| [Oil Find](https://github.com/oil-oil/oil-find/tree/36559e11812d65df01ffd0de132ba1a9a278701d) | `36559e11812d65df01ffd0de132ba1a9a278701d` | [MIT，Lin Zhihuang](https://github.com/oil-oil/oil-find/blob/36559e11812d65df01ffd0de132ba1a9a278701d/LICENSE) |
| [DigUp](https://github.com/ARahim3/DigUp/tree/6faa29e4c315a3a591de1b41bd78751bdf39588d) | `6faa29e4c315a3a591de1b41bd78751bdf39588d` | [MIT，Abdur Rahim](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/LICENSE) |
| [Tinycast](https://github.com/abue-ammar/tinycast/tree/888b5e59fc39f62c7c42ad6ab42f689e6b233eeb) | `888b5e59fc39f62c7c42ad6ab42f689e6b233eeb` | [AGPL-3.0-or-later，Abue Ammar](https://github.com/abue-ammar/tinycast/blob/888b5e59fc39f62c7c42ad6ab42f689e6b233eeb/LICENSE) |

## 2. 直接复用或小幅裁剪

“直接复用”指保留原语言和关键算法，增加调用边界或裁剪功能，不等于原样运行整套应用。

| 来源 | 可复用内容 | 第一版处理 |
| --- | --- | --- |
| Oil Find [`coilfind.c`](https://github.com/oil-oil/oil-find/blob/36559e11812d65df01ffd0de132ba1a9a278701d/Sources/COilFind/coilfind.c) | `getattrlistbulk` 批量目录读取、NEON 名称子串匹配、候选批量筛选/评分 | 保留 C，Rust FFI 调用；复核内存所有权、返回容量和取消边界 |
| Oil Find [`coilfind.h`](https://github.com/oil-oil/oil-find/blob/36559e11812d65df01ffd0de132ba1a9a278701d/Sources/COilFind/include/coilfind.h) | 扫描及匹配的现成 C ABI | 引入实际用到的接口，不先搬 `bloom.c` 等未用组件 |
| DigUp [`Extractors.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/DigUpKit/Extractors.swift) | PDFKit 分页文本、Cocoa 文档解析 | 裁剪成 Swift 文本 helper；移除 OCR/媒体分支，TXT/MD 留 Rust；改为返回原文与定位 |
| DigUp [`LlamaEmbedder.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/LlamaRuntime/LlamaEmbedder.swift) | EmbeddingGemma 2 的 token framing、pooling、Metal 批处理、归一化 | 保留 Swift + llama.cpp；分离 text-only 权重与 query/document context 预算；不能原样编码长文档 |
| DigUp [`build-llama.sh`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/scripts/build-llama.sh) / [`llama.cpp-no-logits.patch`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/scripts/llama.cpp-no-logits.patch) | 已固定的 llama.cpp `b11461` 与避免无用 logits 内存的补丁 | 复用构建配置/补丁，改成本项目目录；重新核对构建依赖许可证，不复制脚本的强制 checkout 行为 |
| DigUp [`Model.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/DigUpKit/Model.swift) / [`ModelFiles.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/DigUpKit/ModelFiles.swift) | 官方 query/document prompts、模型 revision/文件信息 | 按固定模型卡使用提示；初版只取文本权重，下载目录改为 Luminator；不复制整个多模态配置 |
| DigUp [`ReferenceVectorTests.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Tests/LlamaRuntimeTests/ReferenceVectorTests.swift) | 推理运行时向量一致性检查和参考向量方法 | 后续复用测试资产并保留来源，新增中文样例；无模型而 skip 不算运行通过 |

DigUp 的 `Worker.swift` JSON Lines 结构可作为 helper 通信参考；它的 Swift 父进程管理代码不直接变成 Rust daemon。Oil Find 的 CoreFoundation 拼音变换可以保留小型原生桥，Rust 管理拼音键和索引，不为第一版自建汉字读音数据库。

## 3. 用 Rust 重写 / 自己实现

| 来源或问题 | Rust 的职责 | 为什么不能直接搬 |
| --- | --- | --- |
| Oil Find [`IndexStore.swift`](https://github.com/oil-oil/oil-find/blob/36559e11812d65df01ffd0de132ba1a9a278701d/Sources/OilFindCore/IndexStore.swift) | 紧凑 SoA、名称池、父节点、稳定文件 ID、快照生命周期 | Swift 指针管理和应用对象不适合作为独立 Rust 引擎状态；借鉴布局 |
| Oil Find [`Searcher.swift`](https://github.com/oil-oil/oil-find/blob/36559e11812d65df01ffd0de132ba1a9a278701d/Sources/OilFindCore/Searcher.swift) | 查询规范化、候选收窄、过滤、名称排序 | C 内核复用，Swift 查询编排重写；保留缓存失效语义 |
| Oil Find [`FSWatcher.swift`](https://github.com/oil-oil/oil-find/blob/36559e11812d65df01ffd0de132ba1a9a278701d/Sources/OilFindCore/FSWatcher.swift) / [`IndexUpdater.swift`](https://github.com/oil-oil/oil-find/blob/36559e11812d65df01ffd0de132ba1a9a278701d/Sources/OilFindCore/IndexUpdater.swift) | FSEvents C API 封装、游标恢复、补扫、批量更新、取消 | 借鉴事件丢失恢复和扫描期缓冲，不复制 Swift owner/队列结构 |
| DigUp [`Indexer.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/DigUpKit/Indexer.swift) | 名称/提取/全文/向量任务、generation、分步长文件、暂停恢复 | 借鉴状态和长文件策略；不能让每次变化都依赖全根目录同步 |
| DigUp [`IndexStore.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/DigUpKit/IndexStore.swift) | SQLite 事务、文件/片段/任务、FTS5 和向量持久化 | 参考表的业务含义，按我们的名称索引/中文需求建简化表，不做数据库格式兼容 |
| DigUp [`Vectors.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/DigUpKit/Vectors.swift) / [`Searcher.swift`](https://github.com/ARahim3/DigUp/blob/6faa29e4c315a3a591de1b41bd78751bdf39588d/Sources/DigUpKit/Searcher.swift) | FP16 连续向量缓存、Accelerate C ABI 调用、Top-K、按文件聚合、RRF | 不搬 Swift 数据管理；DigUp 的全量分数归一化不能直接用于未来 ANN |
| 两个应用都没有的需求 | daemon/CLI/IPC、中文双字候选与原文验证、分阶段响应、跨客户端配置 | 是新的用户需求；保持一条本地调用链，不另建通用框架 |

SQLite/llama.cpp/Accelerate 本身不用 Rust 重写。Tinycast/Raycast 的界面分别用 Swift/TypeScript；这些客户端不重新实现索引。

## 4. 不能当成现成能力的部分

- **Oil Find Pro 的内容搜索没有在当前公开源码中提供**，不能把购买版功能写成可直接复用的 MIT 模块。
- **云文件覆盖不是换扫描器就能解决。** Oil Find 的 dataless 目录分支不继续下降，DigUp 也避开未下载内容；保留本地读取边界并明确缺口。
- **DigUp 的中文全文需要改。** 本地 SQLite 最小复现使用其 tokenizer：“医院合作分工方案”整词命中，搜“合作分工”不命中。这是词法索引问题，不能据此判断语义模型不好。
- **字符长度不是 token 长度。** DigUp 的固定字符分块和当前 helper context 必须重新协调；不能偷偷截掉中文正文末尾后报“已索引”。
- **mmap 不等于零拷贝。** Oil Find 的 `mappedIfSafe` 后仍复制入内存数组；DigUp 向匿名 mmap 缓冲区填入 SQLite 向量。本方案没有宣称它们已经提供文件映射式零拷贝索引。
- **Swift 运行时可裁剪，不等于已经独立可编译。** 从 DigUp 拆 helper 需要整理 DigUpKit/CLlama 依赖闭包和去掉媒体引用，实施时编译与参考向量验证；当前只做了源码可行性判断。

## 5. 许可证与仓库安排

Luminator 原创核心、CLI、客户端接口和自行编写的 Raycast 扩展采用 MIT。MIT 允许商业使用和出售副本；采用 MIT 的同时也允许别人 fork 和商用，商业收入不能依赖源码排他性。

引入 Oil Find / DigUp 代码时保留原版权和完整 MIT 文本，新增第三方来源说明，记录复制/修改的文件与固定提交。当前只有设计，没有提前标记“已 vendor”。llama.cpp 及实际构建依赖的 notices 跟随发布产物；EmbeddingGemma 2 模型卡标注 [Apache-2.0](https://huggingface.co/google/embeddinggemma-2)，模型与 GGUF 权重另行分发，不塞入 MIT 源码仓库。

Tinycast 当前是 AGPL-3.0-or-later。其源文件和修改版继续在 Tinycast fork 中保留该许可证，不能复制后换成 MIT。MIT 引擎用公开的本地进程接口独立提供服务；这是工程分离方案，不将“独立进程”视作可以自动免除一切组合分发义务的法律结论。Raycast 是外部宿主，本项目仅使用其公开扩展 API，不复制其闭源应用代码。

建议仓库安排：

1. `Surfire/luminator-index`：个人公开主仓库，MIT，引擎、文档和 Raycast 扩展。
2. Tinycast fork：只有开始集成且需要推送时才建立，AGPL，尽量把通用 backend 接口贡献上游；当前不建立第二个仓库。
3. 不 fork Oil Find / DigUp 整个产品；只按需要引入有来源的组件，保持上游版本可追溯。

2026-10-12 经用户确认，已建立个人公开仓库 `Surfire/luminator-index`，先发布方案文档，接受对接口、中文需求和性能口径的反馈。README 保留“设计阶段”状态，不以尚未测量的性能作宣传。本次没有发布实现代码、模型权重或用户资料。
