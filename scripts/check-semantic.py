"""Real-runtime integration checks using disposable synthetic documents."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument('--engine', required=True)
p.add_argument('--extractor', required=True)
p.add_argument('--embedder', required=True)
p.add_argument('--model', required=True)
a = p.parse_args()
a.engine, a.extractor, a.embedder, a.model = [str(Path(x).resolve()) for x in (a.engine,a.extractor,a.embedder,a.model)]

def ipc(sock, payload):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(30)
        s.connect(str(sock))
        s.sendall((json.dumps(payload,ensure_ascii=False)+'\n').encode())
        b=b''
        while not b.endswith(b'\n'):
            data=s.recv(65536)
            if not data: raise RuntimeError('socket closed')
            b+=data
    reply = json.loads(b)
    if "error" in reply: raise RuntimeError(reply["error"])
    return reply

def until(check, seconds=30):
    end=time.monotonic()+seconds
    while time.monotonic()<end:
        if check(): return
        time.sleep(.1)
    raise AssertionError('condition timed out')

with tempfile.TemporaryDirectory(prefix='lumi-sem-',dir='/tmp') as tmp:
    base=Path(tmp).resolve(); root=base/'documents';root.mkdir()
    names=base/'names.sqlite'; content=base/'names.content.sqlite'; vectors=base/'names.semantic.sqlite';sock=base/'s.sock'
    pairs=[
        ('离职同事的账号怎样处理','员工离职后应停用账户、撤销权限并交接文件。'),
        ('出差花的钱怎么报回来','差旅费用报销需提交车票、酒店发票及审批记录。'),
        ('断电以后如何找回资料','备份恢复流程包括检查备份完整性和还原文件。'),
        ('一个病人来了多次怎么避免重复统计','数据分析按患者唯一标识去重，多次就诊合并计算人数。'),
        ('供应商什么时候能收到款','采购合同约定验收通过后十个工作日支付尾款。'),
        ('会议讨论结果由谁跟进','会议纪要应记录决议、责任人、交付日期和未解决问题。'),
    ]
    for i,(_,text) in enumerate(pairs): (root/f'{i}.txt').write_text(text)
    def cli(args): return json.loads(subprocess.check_output([a.engine]+[str(x) for x in args]))
    cli(['--db',names,'scan',root])
    def extract(): cli(['--db',content,'content','scan','--names-db',names,'--helper',a.extractor,'--enable'])
    extract()
    def launch(paused=False):
        args=[a.engine,'--db',str(vectors),'semantic','--content-db',str(content),'--helper',a.embedder,'--model',a.model,'--socket',str(sock),'--enable']
        proc=subprocess.Popen(args+(['--paused'] if paused else []),stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        until(lambda:sock.exists() or proc.poll() is not None)
        if proc.poll() is not None: raise RuntimeError(proc.stderr.read().decode())
        return proc
    proc=launch(True)
    try:
        assert ipc(sock,{})['indexed']==0
        time.sleep(.4);assert ipc(sock,{})['indexed']==0
        ipc(sock,{'paused':False})
        until(lambda:ipc(sock,{})['indexed']==6)
        hits=[]
        for i,(q,_) in enumerate(pairs):
            r=ipc(sock,{'query':q})['results']
            assert r and r[0]['path']==str(root/f'{i}.txt'), (i,r)
            assert r[0]['match_kind']=='semantic'
            hits.append(True)
        before=ipc(sock,{'paused':True})['indexed']
        (root/'added.txt').write_text('图书馆借阅图书逾期需要归还，并按规定办理续借手续。')
        extract(); time.sleep(.5)
        assert ipc(sock,{})['indexed']==before
        assert ipc(sock,{'query':pairs[0][0]})['results'], 'paused indexes remain searchable'
        proc.terminate();proc.wait(timeout=10)
        proc=launch(True)
        assert ipc(sock,{})['indexed']==before, 'restart keeps completed work'
        ipc(sock,{'paused':False}); until(lambda:ipc(sock,{})['indexed']==7)
        # Hide obsolete results immediately, before the content index catches up.
        target=root/'0.txt';target.write_text('太阳能电池将光能转化为电能，可用于屋顶发电系统。')
        assert all(x['path']!=str(target) for x in ipc(sock,{'query':pairs[0][0]})['results'])
        extract()
        until(lambda:any(x['path']==str(target) for x in ipc(sock,{'query':'屋顶如何利用日光发电'})['results']))
        renamed=root/'renamed.txt';target.rename(renamed)
        assert all(x['path']!=str(target) for x in ipc(sock,{'query':'屋顶如何利用日光发电'})['results'])
        extract()
        until(lambda:any(x['path']==str(renamed) for x in ipc(sock,{'query':'屋顶如何利用日光发电'})['results']))
        renamed.unlink()
        assert all(x['path']!=str(renamed) for x in ipc(sock,{'query':'屋顶如何利用日光发电'})['results'])
        extract()
        until(lambda:ipc(sock,{})['indexed']==6)
        print(json.dumps({'semantic_top1':len(hits),'pause_resume':True,'restart_resume':True,'modify_rename_delete':True,'stale_filtered_before_rescan':True}))
    finally:
        proc.terminate();proc.wait(timeout=10)
