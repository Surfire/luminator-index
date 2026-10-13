# 可选推理后端与首轮实验（2026-10-13）

建议：名称、全文继续独立运行；语义使用可选的独立推理进程。
默认候选为 DigUp 路线的 llama.cpp / Metal，预留外部 OpenAI embeddings 接口。
本轮只完成推理实验，没有建立真实资料库的向量索引，也没有安装系统后台服务。

## 本机核对

| 后端 | 核对结果 | 建议 |
| --- | --- | --- |
| oMLX 0.7.0 | 已安装源码有 `/v1/embeddings`；本机模型列表没有 embedding 模型；所带 mlx-embeddings 没有 `embedding_gemma2` / `embedding_gemma2_text` 实现 | 保留为外部后端候选，目前不能直接运行该模型 |
| Splash 1.0.1 | 已安装服务端 POST 路由没有 `/v1/embeddings`；现有服务提供 Qwen 生成模型 | 以后用于摘要、问答 |
| llama.cpp b11461 / Metal | 已加载下载的 GGUF，输出规范化 768 维向量 | 首选实验路线，按需启动和退出 |

Google 的模型配置为 `EmbeddingGemma2Model` / `embedding_gemma2`，文本模型类型为
`embedding_gemma2_text`，不能把第一代 Gemma3 embedding 支持视为二代支持。
来源：https://huggingface.co/google/embeddinggemma-2/blob/main/config.json
本机核对仅代表上述安装版本，不推断未来上游支持。未升级、停止或修改 oMLX / Splash 服务。

## 可复现测试

M5 Max，本地 Q8_0（309,855,456 bytes），同机有后台编译任务。
模型版本及 SHA256 见 `embeddinggemma-2-manifest.json`。
构建 `scripts/build-text-model.sh`，运行：

```sh
uv run --no-project python3 scripts/benchmark-text-model.py \
  --helper target/release/LuminatorTextEmbedder --model /path/to/model.gguf \
  --output /private/path/summary.json
```

- 已有文件和 Metal 缓存时，新进程加载约 652 ms，首条查询往返 17.5 ms。
  构建后的首次运行另观察到加载约 3.14 s、首批两条编码约 631 ms。
- 36 次短查询往返 p50 6.59 ms、p95 10.22 ms；纯编码 p95 9.36 ms。
- 12 条短文档、300 tokens 顺序编码约 359.5 ms；不是 GPU 并行批处理吞吐。
- 12 组合成中文改写查询对 12 个候选：768 维和截短后重新归一化的 256 维均 top-1 12/12。
  这是协议及语义方向的 smoke test，不代表真实资料库准确率。
- 查询后进程 RSS 约 483 MiB；不等于整机 GPU 峰值或系统总增量内存。
- 模长接近 1；重复查询一致；超过实验配置 2048 tokens 被拒绝，随后仍可查询。
  2048 是 worker 资源限制，不是模型本身最大上下文。
- 聚合结果：`../benchmarks/results/2026-10-13-semantic/summary.json`。

## 接入边界（待实施）

Rust 持有分块和向量索引，worker 只负责文本到向量。语义默认关闭，故障不影响名称和全文。
不把模型加载进名称服务，不重写 llama.cpp / MLX 算子，不另建通用推理框架。
外部后端配置包括 endpoint、模型标识、维数；内置后端配置模型路径并管理子进程生命周期。
query 前缀为 `task: search result | query: ...`，document 为 `title: ... | text: ...`。
索引记录模型版本、维数、量化、前缀和分块版本；模型切换时重建向量，不能混用空间。
下一步先对已提取的正文做有限规模、人工可审阅的真实语义检索评测，再接主搜索融合。
