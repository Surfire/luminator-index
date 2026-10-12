"""Compare filename-only CLI result sets and process round trips on one root.

Queries, baselines and output stay at paths explicitly supplied by the caller.
This measures mdfind, not the Spotlight graphical interface or content search.
"""
import argparse
import json
import math
from pathlib import Path
import random
import subprocess
import time
import unicodedata


def normalized(text):
    return unicodedata.normalize("NFC", text.lower())


def summary(values):
    values = sorted(values)
    return {"samples": len(values), "p50_ms": values[math.ceil(len(values)*.5)-1],
            "p95_ms": values[math.ceil(len(values)*.95)-1], "max_ms": values[-1]}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--root", type=Path, required=True)
    p.add_argument("--db", type=Path, required=True)
    p.add_argument("--inventory", type=Path, required=True)
    p.add_argument("--queries", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--binary", type=Path, default=Path("target/release/luminator"))
    p.add_argument("--repeats", type=int, default=5)
    args = p.parse_args()
    assert args.repeats >= 5
    root = args.root.resolve()
    inventory = json.loads(args.inventory.read_text())["entries"]
    cases = json.loads(args.queries.read_text())
    # Chinese literal queries have identical filename semantics in both engines.
    queries = [c["query"] for c in cases if any("\u4e00" <= x <= "\u9fff" for x in c["query"])]
    assert len(queries) >= 50
    records = []
    all_times = {"luminator": [], "spotlight_mdfind": []}

    def run(which, q):
        if which == "luminator":
            command = [str(args.binary.resolve()), "--db", str(args.db.resolve()),
                       "query", q, "--mode", "filename", "--limit", "10000"]
        else:
            # Escaping the metadata expression keeps *, quotes and backslashes literal.
            escaped = q.replace("\\", "\\\\").replace('"', '\\"').replace("*", "\\*").replace("?", "\\?")
            command = ["/usr/bin/mdfind", "-0", "-onlyin", str(root),
                       f'kMDItemFSName == "*{escaped}*"c']
        start = time.perf_counter()
        completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   check=True, timeout=30)
        elapsed = (time.perf_counter()-start)*1000
        if which == "luminator":
            value = json.loads(completed.stdout)
            assert len(value["results"]) < 10000, "result cap would invalidate comparison"
            paths = {str(Path(h["path"]).relative_to(root)) for h in value["results"]}
        else:
            paths = {str(Path(x.decode()).relative_to(root)) for x in completed.stdout.split(b"\0") if x}
        return elapsed, paths

    rng = random.Random(20261013)
    for q in queries:
        expected = {r["relative"] for r in inventory
                    if r["kind"] <= 2 and normalized(q) in normalized(r["name"])}
        item = {"query": q, "expected": len(expected)}
        for which in all_times:
            run(which, q)  # warm this query once
        samples = {which: [] for which in all_times}
        for _ in range(args.repeats):
            order = list(all_times)
            rng.shuffle(order)
            for which in order:
                elapsed, paths = run(which, q)
                samples[which].append(elapsed)
                item[which] = {"returned": len(paths), "missing": len(expected-paths),
                               "unexpected": len(paths-expected),
                               "missing_paths": sorted(expected-paths)}
        for which in all_times:
            all_times[which].extend(samples[which])
            item[which]["latency"] = summary(samples[which])
        records.append(item)
    output = {"scope": "filename-only full-result CLI; not graphical Spotlight",
              "queries": len(queries), "repeats": args.repeats,
              "latency": {k: summary(v) for k, v in all_times.items()},
              "coverage": {k: {"exact_queries": sum(r[k]["missing"] == 0 and r[k]["unexpected"] == 0 for r in records),
                                "expected_query_file_pairs": sum(r["expected"] for r in records),
                                "missing_query_file_pairs": sum(r[k]["missing"] for r in records),
                                "unexpected_query_file_pairs": sum(r[k]["unexpected"] for r in records)} for k in all_times},
              "records": records}
    args.output.write_text(json.dumps(output, ensure_ascii=False, indent=2)+"\n")
    print(json.dumps({k: v for k, v in output.items() if k != "records"}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
