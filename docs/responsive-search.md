# 输入响应、批量更新与候选检索实测

2026-10-13，Apple M5 Max / 128 GB / macOS 27.0.1。引擎基线 `3f6a45c`，Tinycast 基线 `2325dff`。
当前改动只包含文件名引擎和 Tinycast 查询等待。正文和模型均未参与。

## 改动与结果

| 场景 / 指标 | 修改前 | 修改后 |
| --- | ---: | ---: |
| 私有真实索引，Swift 查询 session 到结果发布，p50 | 27.88 ms | 10.69 ms |
| 同上 p95，60 次 / 版本 | 28.96 ms | 11.54 ms |
| 百万索引，连续输入后续按键 socket p50 | 3.90 ms | 1.18 ms |
| 同上 p95，360 次 / 版本 | 4.72 ms | 1.34 ms |
| 百万索引，连续输入首段 socket p95，80 次 / 版本 | 3.93 ms | 3.78 ms |
| 百万索引，批量更新期间查询 p95，先旧后新 | 72.56 ms | 4.52 ms |
| 同上交换测试顺序，先新后旧 | 71.02 ms | 4.82 ms |
| 批量更新期间最大查询耗时，两轮各自最大 | 76.60 ms | 5.77 ms |
| 1 万条批次全部可见 p95，先旧后新 | 85.80 ms | 99.12 ms |
| 百万索引，连续输入测试的服务物理内存峰值 | 109.20 MiB | 205.58 MiB |

固定输入合并窗口从 25 ms 改为 8 ms，真实 session p95 改善约 **2.5 倍**。
连续输入后续查询 p95 改善约 **3.5 倍**；首段仍需全量扫描，不能称所有查询都快 3.5 倍。
批量更新期间查询 p95 在交换顺序的两轮中改善约 **15–16 倍**。
这些倍数不能相乘。没有测量 SwiftUI 帧呈现时间，也没有重新比较 Spotlight GUI。

内存是明确的取舍：启动时保留两份索引，使百万场景服务峰值比旧版增加约 **96 MiB**，
**超过此前 150 MiB 峰值目标**；因此本次不宣称全部资源指标达标。查询继续使用提交前版本，
批量变更提交后再发布，新文件最终可见时间略增加。优先保证交互响应，后续再以结构共享减少副本，
不为降低内存而把磁盘扫描重新放回查询线程。

## 如何复测

### 真实 session

在真实 OneDrive 索引的本机 SQLite 副本上，使用同一个旧版 daemon，保持后端不变，
只比较旧 / 新 `LuminatorSearchSession.swift`。使用 Tinycast 的实际 `LuminatorClient.query`、
JSON 解码和结果发布路径，3 次预热后测 60 次。只读名称与元数据，不打开正文。
测量包含合并等待、socket 和解码，以 100 μs 间隔等候发布，**不包含 SwiftUI 绘制**。
实现位于 Tinycast 本地分支的 `Scripts/luminator-latency.swift`，参数为 socket 与至少两个不同查询。
编译时链接 `LuminatorSearchSession.swift`、`LuminatorClient.swift`、`LuminatorReply.swift`、
`FileSearchResult.swift`、`AppPaths.swift`、`ProcessExit.swift`；基线仅替换 session 源文件。
公开记录只有统计，不包含私有查询、路径或索引。

### 候选缓存

使用同一个百万条目合成数据库的两份 SQLite 副本，旧、新 daemon 顺序运行。
四组中文、英文、拼音连续输入序列重复 20 次，每轮先发送无关查询使候选缓存失效。
总计 440 次 / 版本，其中首段 80 次、连续延长 360 次。比较每次查询的全部返回 ID、
分数和命中类型，**440 / 440 的结果与排序完全一致**。

```sh
uv run --no-project python3 benchmarks/responsive.py \
  --mode typing --before /path/to/before --after target/release/luminator \
  --db benchmarks/local-results/20261013/million.sqlite --output /tmp/typing.json
```

缓存保存匹配阶段的**全部候选**，不受 Top-K、类型、扩展名、范围筛选影响。
只在规范化查询连续延长且匹配模式一致时复用。路径查询、退格/改词、模式改变走完整扫描；
索引发布任何变更即清空。测试额外覆盖 7 万条目、前一轮 Top-1 之外的目标和筛选变化。

### 批量更新

使用同一百万条目实际合成目录树，每个版本单独复制基线数据库。
在该树中搬入独立测试目录，内含 10,000 个空文件；持续查询既有名称及新增名称，
直到新增目录末条记录可见，然后移走并等待全部删除可见。每个版本做 10 轮。
最后恢复原来 1,000,000 条可检索记录；未修改用户资料。
更新窗口内两种查询的完整 socket 往返均计入，包含连接、JSON 编解码。
旧版因阻塞完成的请求更少，因此两侧样本数不同；原始样本全部保留，没有删除慢样本。
第二轮交换新旧顺序，结果方向一致。测试使用热缓存，不声称冷磁盘或不同 Mac 上相同。

```sh
uv run --no-project python3 benchmarks/responsive.py \
  --mode bulk --before /path/to/before --after target/release/luminator \
  --db benchmarks/local-results/20261013/million.sqlite --output /tmp/bulk.json
# 加 --reverse 交换顺序；不传 --db 时使用独立的小索引。
```

## 调度与一致性

Rust 写入线程独占 Catalog、FSEvents 和写入索引，执行磁盘扫描及 SQLite 事务。
查询线程独占自己的索引与候选缓存，没有查询期间的数据库 I/O，也没有与写入线程共享的写锁。
启动时复制一次索引；以后只在事务提交成功后传递按 ID 排序的变更记录，包含新增、改名、
元数据和删除标记。容量为 1 的通道限制待发布批次数。失败会退出，不发布未提交数据；
重启从已提交 SQLite 恢复。普通退出会停止并等待写入线程。

这不是零阻塞承诺：查询线程应用变更记录仍是一个同步步骤；本次测了每批 1 万记录，
没有验证一次发布百万条变更的极端停顿。启动恢复仍先读取数据库；历史失效时的启动全扫描
尚未改成可查询状态。单个连接读取请求仍有 1 秒超时，慢客户端隔离也不在本次范围。

## 其余回归

Rust 单元与集成测试、fmt、严格 clippy 通过，包含 FSEvents、离线恢复、Unicode/多音字、
根目录、目录重命名/删除、单写入者、分段 socket 请求以及候选正确性。
Tinycast 的 `luminator-test`、`file-search-session-test`、Debug / Release 构建与 lint 通过。
本轮不重新运行会打开原生窗口的整套 UI harness，遵守后台验证要求；之前原版亦失败的
`form-input-test` 不作为本次搜索优化通过的证据。

百万索引新版本单文件新增/改名/修改/删除各 30 次，p95 分别为 **29.45 / 30.71 / 18.44 / 27.97 ms**。
停止期间变更恢复到可检索约 **592 ms**。60 秒空闲 CPU 约 **0.28% 单核**；
空闲采样后段与构建任务重叠，不称整机严格空闲。服务峰值约 **202 MiB**，符合本轮双索引代价。

体验版已替换为本轮 Release，签名校验通过，安装后的主程序和 helper 与构建产物哈希一致。
使用安装后的 helper 和真实资料库后台复核，中文、全拼和首字母均将索引根目录排在第一位；
实际 Swift session 再测 60 次，p95 为 **11.32 ms**，随后体验版已在后台重启。

原始统计：[session](../benchmarks/results/2026-10-13-responsive/session.json)、
[连续输入](../benchmarks/results/2026-10-13-responsive/typing.json)、
[批量更新](../benchmarks/results/2026-10-13-responsive/bulk.json)、
[交换顺序](../benchmarks/results/2026-10-13-responsive/bulk-reverse.json)、
[增量和资源](../benchmarks/results/2026-10-13-responsive/live.json)。
