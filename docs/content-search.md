# 已下载文档的中文全文搜索

2026-10-13 实现；名称引擎保持独立。全文通道没有向量检索、问答或 OCR；独立模型实验另见 [后端报告](inference-backends.md)。

## 运行与开关

```sh
cargo build --release --locked
scripts/build-content-helper.sh
# names.sqlite 是已经明确选择一个根目录建立的名称索引。
# 正文数据库必须是另一份本地文件，位于源目录以外。
target/release/luminator --db /tmp/content.sqlite content scan \
  --names-db /tmp/names.sqlite --helper "$PWD/target/release/LuminatorContentExtractor" --enable
target/release/luminator --db /tmp/content.sqlite content query '合作方案'
target/release/luminator --db /tmp/content.sqlite content status
# 独立正文进程；每轮完成后等 5 秒，再检查名称元数据，只有变化文件重提取。
target/release/luminator --db /tmp/content.sqlite content serve \
  --names-db /tmp/names.sqlite --helper "$PWD/target/release/LuminatorContentExtractor" \
  --socket /tmp/luminator-content.sock --enable
```

不传 `--enable` 不读取正文、不建库。SIGTERM/Ctrl+C 停止索引和查询并结束提取子进程，保留本地索引。
不安装登录服务。`content query/status` 只读已有数据库，不隐式创建索引。
索引包含原文片段，放在用户本地目录、文件权限 0600，不进入 Git。
关闭功能不会擦除片段；删除内容库之前须停止对应进程。

Tinycast 本地体验分支的 Settings → File Search → Luminator 中新增
**Search downloaded document text**，默认关闭，备份及 `settings.json` 均不携带此授权。
全局开关配合每目录的“正文”勾选生效；新增目录默认仅名称。
正文库位于名称库旁：`names.sqlite` → `names.content.sqlite`。
第一次输入搜索后启动正文进程，关闭开关或退出应用停止它。
名称保留 8 ms 输入合并窗口；正文独立等待 150 ms 再查询，按路径去重后补在名称结果后。
结果副标题标记“正文”，给出命中附近片段；Enter 打开文件，暂不跳转 PDF 页码。
后台索引期间，同一条未改变的输入不会自动轮询刷新；重新输入即可查询新片段。

## 提取与中文匹配

- TXT / Markdown：UTF-8，最多 8 MiB；其他编码不猜测转换。
- PDF：最多 64 MiB，PDFKit 文本层；保留真实页码，不渲染页面、不 OCR。
- DOCX：最多 64 MiB，Cocoa officeOpenXML 解析段落及表格文本，保留段落号，无虚构页码。
- PDF/DOCX 的提取文本上限 8 MiB，达到上限标记 `partial_limit`；每个 helper 最长 30 秒。
- 隐藏文件、符号链接、app/bundle/framework 跳过。Excel、PPT、图片、旧 `.doc` 尚不支持。

Rust 管理 SQLite WAL、增量指纹、FTS5 和查询；Swift helper 复用 DigUp 的原生提取方式。
展示片段目前也做 NFC/小写规范化；完整原文仍以源文件为准。
查询先做 NFC/小写/忽略空白，使用单字和相邻双字检索键寻找候选，再做完整子串验证。
支持句中中文词和单字；不是分词语义匹配，不支持正文拼音、AND/OR、模糊纠错。
每块最多 1,024 个非空白字符，相邻块重叠 128；单次查询最多 128 个非空白字符。
PDF 每页 / DOCX 每段独立，不承诺跨页或跨段短语匹配。
每文件只返回一个片段，首版按路径稳定排序，不把排序称为相关性评分。

正文用独立数据库、进程、socket 和写线程；查询用只读连接，名称请求不等待正文。
修改指纹含设备、inode、大小、纳秒 mtime/ctime、文件 flags。提取前后核对；文件已变动
则不发布旧内容，下一轮重试。查询还核对当前元数据，删除/已变化/云端化的文件立即不返回旧片段。
删除的数据库记录在下一轮扫描清理；单个文件读取失败不阻塞其他文件。

## 不触发云文件下载

Rust 进程及 Swift helper 都先设置进程级
`IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES / IOPOL_MATERIALIZE_DATALESS_FILES_OFF`；
设置失败即拒绝运行。同时检查 `SF_DATALESS`，跳过云文件/云目录；实际打开使用 `O_NOFOLLOW`。
即使检查与打开之间发生云盘回收，也由内核拒绝物化，而不是自动下载。
依据：[Apple TN3150](https://developer.apple.com/documentation/technotes/tn3150-getting-ready-for-data-less-files)。
对不可枚举目录统计缺口；不能据此宣称已覆盖云端所有文件。

## 真实目录验收（仅统计公开）

用户授权的一个 OneDrive 根目录，只读已下载文件。Apple M5 Max；本地缓存和 OS 调度影响结果。
源文件及测试查询片段留在私有目录，公开仓库不包含这些内容。

| 指标 | 实测 |
| --- | ---: |
| 首次提取与建库 | 44.93 秒 |
| 成功提取文档 | 1,163 |
| 云文件跳过 | 279 |
| 无文本层/空文本 | 117，其中 PDF 114 |
| 超过输入限制 / 不可读 | 3 / 2 |
| 不支持格式 | 1,606 |
| 片段 / 正文数据库 | 75,530 / 220.69 MiB |
| 中文来源文件找回 | 60/60，四种格式各 15 条 |
| 结果与完整子串 SQL 对照一致 | 60/60，无错误命中 |
| 合成无答案查询 | 20/20 返回空 |
| 以上 80 次正文 socket 往返 p50 / p95 | 2.02 / 2.99 ms |
| 常见短词及单字，5 次重复 | 中位数 2.5–43.6 ms，最高 47.1 ms |
| 建库期间名称 socket p95 / 完成后 p95 | 0.74 / 0.78 ms，各 500 次 |
| 隔离测试目录新增/修改/改名可见 | 5.20 / 5.16 / 5.18 秒 |
| 正文进程停止 | 127 ms |

中文查询为固定种子抽取的正文连续片段，验证字面检索；不是人工语义相关性评测。
名称初次测量 p95 1.65 ms，受预热影响，不把期间 0.74 ms 宣称为正文使名称更快。
真实源目录前后 3,170 个文件的大小、mtime 未变化，观察到云→本地转换 0 个。
另对 5 个实际云端 PDF/DOCX 跑 helper，全部拒绝读取并返回空片段。
修改/改名/删除测试在隔离合成目录完成，没有改动真实资料。

正文增量目前约 5 秒轮询，明显慢于名称 FSEvents 通道；下一轮可改事件驱动，不影响现有名称速度。
本轮未用 GUI 自动化测试，不夺取桌面焦点；Swift 解码/会话/选择保持用无界面 harness 验证。

## 文本模型实验已跑通，尚未接入搜索

已按 DigUp `ModelFiles.swift` 的固定清单下载并校验 EmbeddingGemma 2 Q8_0 文本 GGUF：
309,855,456 字节，SHA-256 `2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135`。
来源为 hf-mirror.com，revision `bfcd298762cc34d0357ece5ebdd31791a3a374d8`，清单在
[`docs/embeddinggemma-2-manifest.json`](embeddinggemma-2-manifest.json)。未下载多媒体权重。
本机权重位于 `~/.local/share/luminator/models/embeddinggemma-2/`，没有随应用或源码分发。
独立推理、合成中文样例和速度测试见 [后端报告](inference-backends.md)；真实语义质量仍待验收。
下一阶段沿用 [模型接入方案](semantic-next-stage.md) 的独立 worker 与可选语义开关。


## 多目录体验配置更新

Tinycast 本地分支现在支持目录列表、添加/移除/重建，以及每个目录的全文勾选。
每个根目录独立数据库和服务，名称并行查询后按路径去重；全局全文开关仍须开启。
新增目录默认仅名称，移除目录停止服务但保留数据库。索引保存位置设置只影响以后新增目录。
界面与代码没有个人路径常量；本机初始目录通过本地设置配置。

本轮在四个新增目录中索引 449,307 条元数据；五库并行的 30 次查询往返
p50 1.13 ms、p95 1.80 ms、最大 309 ms（含首次较慢查询），不含 UI 时间。
新版协调器的后台集成探针通过中文、全拼、首字母根目录命中、跨库路径去重与正文查询。
所有 CLI 入口均启用内核禁止云文件物化策略；只枚举已可见元数据，不自动下载正文。
