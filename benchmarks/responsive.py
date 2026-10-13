"""Compare supplied before/after binaries on an isolated synthetic tree and saved index.
No user tree mutations: bulk fixtures are created under a new temporary directory.
"""
import argparse
import json
import math
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
from contextlib import contextmanager


def ask(sock, query=None, **options):
    start = time.perf_counter()
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(30)
        client.connect(str(sock))
        client.sendall((json.dumps(dict(query=query, **options))+"\n").encode())
        data = json.loads(client.makefile('rb').readline())
    assert 'error' not in data, data
    return data, (time.perf_counter()-start)*1000


def summary(values):
    values = sorted(values)
    return dict(samples=len(values), **{f'p{n}_ms': values[math.ceil(len(values)*n/100)-1]
                                      for n in [50, 95, 99]}, max_ms=values[-1])


def copydb(source, destination):
    with sqlite3.connect(source.as_uri()+'?mode=ro', uri=True) as src, sqlite3.connect(destination) as dst:
        src.backup(dst)


@contextmanager
def daemon(binary, db, folder):
    sock = folder/'engine.sock'
    with (folder/'daemon.log').open('w') as log:
        proc = subprocess.Popen([str(binary), '--db', str(db), 'serve', '--socket', str(sock)],
                                stdout=subprocess.DEVNULL, stderr=log)
        try:
            deadline = time.monotonic()+30
            while True:
                assert proc.poll() is None, 'daemon exited; inspect log'
                try:
                    ask(sock)
                    break
                except (OSError, ValueError):
                    assert time.monotonic() < deadline, 'startup timeout'
                    time.sleep(.02)
            yield sock
        finally:
            if proc.poll() is None:
                proc.terminate()
                proc.wait(timeout=30)


def bulk(binary, folder, source=None):
    folder.mkdir()
    root = folder/'tree'
    if source:
        with sqlite3.connect(source.as_uri()+'?mode=ro',uri=True) as db:
            root = Path(db.execute("SELECT value FROM meta WHERE key='root'").fetchone()[0])
        assert root.name == 'million-tree' and 'benchmarks' in root.parts, 'synthetic fixture only'
    else:
        root.mkdir()
    staging = folder/'staging'; staging.mkdir()
    if not source: (root/'stable-report.txt').touch()
    for n in range(10000):
        (staging/f'bulkitem_{n:05}.txt').touch()
    db = folder/'names.sqlite'
    if source: copydb(source,db)
    else: subprocess.run([str(binary),'--db',str(db),'scan',str(root)],check=True,stdout=subprocess.DEVNULL)
    latency=[]; visibility=[]; counts=[]
    with daemon(binary,db,folder) as sock:
        initial_entries=ask(sock)[0]['entries']
        for trial in range(10):
            target=root/('bulk-'+folder.parent.name); assert not target.exists(); staging.rename(target)
            start=time.monotonic(); samples=0
            while True:
                answer, elapsed=ask(sock,'report' if source else 'stable-report')
                assert answer['results']
                latency.append(elapsed); samples+=1
                reply,check_ms=ask(sock,'bulkitem_09999')
                latency.append(check_ms)
                if reply['results']: break
                assert time.monotonic()-start<30
                time.sleep(.001)
            visibility.append((time.monotonic()-start)*1000);counts.append(samples)
            target.rename(staging)
            start=time.monotonic()
            while ask(sock,'bulkitem_09999')[0]['results']:
                assert time.monotonic()-start<30
                time.sleep(.01)
        status=ask(sock)[0]
        assert status['entries']==initial_entries
    return dict(query_during_update=summary(latency),visibility=summary(visibility),
                queries_per_update=counts,query_samples_ms=latency,status=status)


def typing(binary, db, folder):
    folder.mkdir(); copy=folder/'names.sqlite'; copydb(db,copy)
    sequences=[['rep','repo','repor','report','report_0','report_00','report_000'],
               ['qing','qingq','qingquan','qingquanz','qingquanzil','qingquanziliao'],
               ['ji','jia','jian','jiank','jianka','jiankang'],['资','资料','资料库']]
    latency=[]; narrowed=[]; first=[]; rows=[]
    with daemon(binary,copy,folder) as sock:
        for _ in range(20):
            for sequence in sequences:
                ask(sock,'reset-candidate-sequence')
                for i,q in enumerate(sequence):
                    reply,ms=ask(sock,q)
                    rows.append((q,[(h['id'],h['score'],h['match_kind']) for h in reply['results']]))
                    latency.append(ms)
                    (first if i==0 else narrowed).append(ms)
        status=ask(sock)[0]
    return dict(all=summary(latency),first=summary(first),continued=summary(narrowed),status=status),rows


def main():
    p=argparse.ArgumentParser()
    p.add_argument('--before',type=Path,required=True);p.add_argument('--after',type=Path,required=True)
    p.add_argument('--mode',choices=['bulk','typing'],required=True)
    p.add_argument('--reverse',action='store_true')
    p.add_argument('--db',type=Path);p.add_argument('--output',type=Path,required=True)
    args=p.parse_args();result={}; allrows=[]
    with tempfile.TemporaryDirectory(prefix='lumi-responsive-',dir='/tmp') as work:
        for label in (['after','before'] if args.reverse else ['before','after']):
            binary=getattr(args,label).resolve(); folder=Path(work)/label
            if args.mode=='bulk': result[label]=bulk(binary,folder,args.db.resolve() if args.db else None)
            else:
                result[label],rows=typing(binary,args.db.resolve(),folder);allrows.append(rows)
        if allrows:
            assert allrows[0]==allrows[1], 'candidate cache changed results or ranking'
            result['ordered_results_equal']=True
    args.output.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result))

if __name__=='__main__': main()
