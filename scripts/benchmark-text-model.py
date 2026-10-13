"""Bounded, synthetic Chinese retrieval smoke test; not a corpus quality benchmark."""
import argparse
import json
import math
import statistics
import subprocess
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--helper', required=True)
parser.add_argument('--model', required=True)
parser.add_argument('--output', required=True)
a = parser.parse_args()
pairs = [
    ('离职同事的账号怎样处理', '员工离职后应停用账户、撤销权限并交接文件。'),
    ('出差花的钱怎么报回来', '差旅费用报销需提交车票、酒店发票及审批记录。'),
    ('断电以后如何找回资料', '备份恢复流程包括检查备份完整性和还原文件。'),
    ('一个病人来了多次怎么避免重复统计', '数据分析按患者唯一标识去重，多次就诊合并计算人数。'),
    ('怎样知道模型漏掉了多少病例', '召回率以检出的阳性病例数除以全部真实阳性病例数。'),
    ('找关于保护个人信息的规定', '隐私保护要求对身份证、联系方式等敏感信息脱敏并限制访问。'),
    ('供应商什么时候能收到款', '采购合同约定验收通过后十个工作日支付尾款。'),
    ('怎样让同事找到最新的文档', '文档版本管理规定统一归档目录，注明修订日期并保留历史版本。'),
    ('服务器磁盘快装满了怎么办', '存储维护应清理过期缓存，检查日志保留策略并扩容磁盘。'),
    ('会议讨论结果由谁跟进', '会议纪要应记录决议、责任人、交付日期和未解决问题。'),
    ('如何知道代码改动有没有破坏功能', '软件回归测试检查已有功能在版本更新后是否仍正常工作。'),
    ('怎么给项目排计划', '项目进度表列出任务依赖、负责人、预计工时和里程碑。'),
]
output = Path(a.output)
output.parent.mkdir(parents=True, exist_ok=True)
log = output.with_suffix('.runtime.log').open('w')
p = subprocess.Popen([a.helper, a.model], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True)
def request(texts):
    start = time.perf_counter()
    p.stdin.write(json.dumps({'texts': texts}, ensure_ascii=False) + '\n')
    p.stdin.flush()
    line = p.stdout.readline()
    if not line:
        raise RuntimeError('worker exited')
    return json.loads(line), (time.perf_counter() - start) * 1000
try:
    ready = json.loads(p.stdout.readline())
    cold, cold_wall = request(['task: search result | query: 测试'])
    docs, docs_wall = request(['title: none | text: ' + d for _, d in pairs])
    queries = []
    wall = []
    compute = []
    for _ in range(3):
        for q, _ in pairs:
            r, ms = request(['task: search result | query: ' + q])
            queries.append(r['vectors'][0])
            wall.append(ms)
            compute.append(r['elapsed_ms'])
    def unit(v):
        n = math.sqrt(sum(x*x for x in v))
        return [x/n for x in v]
    quality = {}
    for dim in [768, 256]:
        dv = [unit(v[:dim]) for v in docs['vectors']]
        ranks = []
        for i, qv in enumerate(queries[:len(pairs)]):
            qv = unit(qv[:dim])
            scores = [sum(x*y for x, y in zip(qv, d)) for d in dv]
            order = sorted(range(len(scores)), key=lambda j: -scores[j])
            ranks.append(order.index(i) + 1)
        quality[str(dim)] = {'top1': sum(r == 1 for r in ranks), 'top3': sum(r <= 3 for r in ranks), 'queries': len(pairs)}
    norms = [math.sqrt(sum(x*x for x in v)) for v in docs['vectors'] + queries]
    repeated_delta = max(abs(x-y) for x, y in zip(queries[0], queries[len(pairs)]))
    rss = int(subprocess.check_output(['ps', '-p', str(p.pid), '-o', 'rss=']).strip()) / 1024
    rejected, _ = request(['测' * 10000])
    recovered, _ = request(['task: search result | query: 测试'])
    def stats(values):
        return {'p50_ms': statistics.median(values), 'p95_ms': sorted(values)[math.ceil(len(values)*0.95)-1], 'max_ms': max(values)}
    result = {'runtime': ready, 'cold_query_wall_ms': cold_wall, 'warm_queries': len(wall),
              'warm_query_wall': stats(wall), 'warm_query_compute': stats(compute),
              'document_batch': {'texts': len(pairs), 'tokens': sum(docs['tokens']), 'wall_ms': docs_wall},
              'synthetic_retrieval': quality, 'norm_min': min(norms), 'norm_max': max(norms),
              'repeat_max_abs_delta': repeated_delta, 'rss_mib_after_queries': rss,
              'oversized_input_rejected': 'error' in rejected, 'worker_recovered': 'vectors' in recovered,
              'scope': '12 synthetic pairs, sequential requests, text only; no real-corpus accuracy claim'}
    output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, ensure_ascii=False, indent=2))
finally:
    p.stdin.close()
    try:
        p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p.terminate()
        p.wait(timeout=10)
    log.close()
