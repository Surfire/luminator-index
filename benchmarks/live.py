"""Measure real FSEvents visibility and local socket latency on an existing fixture.

Run with uv run --no-project python3 benchmarks/live.py --help.
Only the explicitly selected synthetic benchmark tree should be used.
"""
import argparse
import json
import math
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time


def request(sock, query=None):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(15)
        client.connect(str(sock))
        client.sendall((json.dumps({"query": query}, ensure_ascii=False) + "\n").encode())
        reply = b""
        while not reply.endswith(b"\n"):
            block = client.recv(65536)
            if not block:
                raise RuntimeError("daemon closed without a reply")
            reply += block
        value = json.loads(reply)
        if "error" in value:
            raise RuntimeError(value["error"])
        return value


def wait_for(sock, query, predicate):
    start = time.perf_counter()
    while time.perf_counter() - start < 10:
        hits = request(sock, query)["results"]
        if predicate(hits):
            return (time.perf_counter() - start) * 1000
        time.sleep(0.01)
    raise RuntimeError(f"change not visible: {query}")


def summary(values):
    values = sorted(values)
    return {"samples": len(values), "p50_ms": values[math.ceil(len(values)*0.5)-1],
            "p95_ms": values[math.ceil(len(values)*0.95)-1], "max_ms": values[-1]}


def cpu_seconds(pid):
    fields = subprocess.check_output(["ps", "-p", str(pid), "-o", "time="], text=True).strip().split(":")
    value = 0.0
    for field in fields:
        value = value * 60 + float(field)
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/release/luminator"))
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--trials", type=int, default=30)
    parser.add_argument("--idle-seconds", type=int, default=60)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    assert args.trials >= 20 and args.idle_seconds >= 60
    root = args.root.resolve()
    assert (root/"group_00000").is_dir(), "requires the synthetic million-tree fixture"
    changes = {key: [] for key in ["create", "rename", "modify", "delete"]}
    output = {"trials": args.trials}
    with tempfile.TemporaryDirectory(prefix="lumi-", dir="/tmp") as run:
        sock = Path(run)/"engine.sock"
        log_path = args.output.with_suffix(".log")
        with log_path.open("w") as log:
            def start():
                proc = subprocess.Popen([str(args.binary.resolve()), "--db", str(args.db.resolve()),
                                         "serve", "--socket", str(sock)], stdout=subprocess.DEVNULL, stderr=log)
                deadline = time.monotonic()+15
                while time.monotonic() < deadline:
                    if proc.poll() is not None:
                        raise RuntimeError(f"daemon exited: {log_path.read_text()}")
                    try:
                        status = request(sock)
                        if status["entries"]:
                            return proc, status
                    except (OSError, ValueError):
                        time.sleep(0.02)
                proc.terminate()
                proc.wait(timeout=10)
                raise RuntimeError("daemon readiness timed out")

            proc, status = start()
            output["entries_before"] = status["entries"]
            try:
                for n in range(args.trials):
                    q = f"实时验证{n:06}"
                    path = root/"group_00000"/f"{q}.txt"
                    renamed = root/"group_00000"/f"改名验证{n:06}.txt"
                    assert not path.exists() and not renamed.exists()
                    path.write_bytes(b"")
                    changes["create"].append(wait_for(sock, q, lambda h: len(h) == 1))
                    path.rename(renamed)
                    renamed_q = f"改名验证{n:06}"
                    changes["rename"].append(wait_for(sock, renamed_q, lambda h: len(h) == 1))
                    wait_for(sock, q, lambda h: not h)
                    renamed.write_bytes(b"metadata-size-update")
                    changes["modify"].append(wait_for(sock, renamed_q, lambda h: h and h[0]["size"] == 20))
                    renamed.unlink()
                    changes["delete"].append(wait_for(sock, renamed_q, lambda h: not h))
                output["changes"] = {key: summary(values) for key, values in changes.items()}
                output["change_samples_ms"] = changes
                timings = []
                for _ in range(100):
                    start_time = time.perf_counter()
                    request(sock, "清泉资料")
                    timings.append((time.perf_counter()-start_time)*1000)
                output["socket_roundtrip"] = summary(timings)
                # Let the last event batch drain before measuring truly idle CPU.
                time.sleep(1)
                begin = cpu_seconds(proc.pid)
                elapsed = time.perf_counter()
                for _ in range(args.idle_seconds//10):
                    time.sleep(10)
                elapsed = time.perf_counter()-elapsed
                output["idle_seconds"] = elapsed
                output["idle_cpu_percent_one_core"] = (cpu_seconds(proc.pid)-begin)/elapsed*100
                output["status_after_updates"] = request(sock)
                proc.send_signal(signal.SIGTERM)
                proc.wait(timeout=10)
                offline = root/"group_00000"/"离线恢复验证.txt"
                assert not offline.exists()
                offline.write_bytes(b"")
                start_time = time.perf_counter()
                proc, _ = start()
                wait_for(sock, "lxhfyz", lambda h: len(h) == 1)
                output["offline_recovery_ms"] = (time.perf_counter()-start_time)*1000
                offline.unlink()
                wait_for(sock, "lxhfyz", lambda h: not h)
                output["entries_after"] = request(sock)["entries"]
                assert output["entries_after"] == output["entries_before"]
            finally:
                if proc.poll() is None:
                    proc.send_signal(signal.SIGTERM)
                    proc.wait(timeout=10)
        output["daemon_startups"] = [json.loads(line) for line in log_path.read_text().splitlines()
                                     if line.startswith('{') and json.loads(line).get("ready")]
    args.output.write_text(json.dumps(output, ensure_ascii=False, indent=2)+"\n")
    print(json.dumps({k:v for k,v in output.items() if k != "change_samples_ms"}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
