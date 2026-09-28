#!/usr/bin/python3
"""Bounded author formatting only; this is not build or qualification evidence."""
import hashlib,json,os,resource,signal,stat,subprocess,time
from pathlib import Path
W=Path(__file__).resolve().parent
paths=['reverie-ptrace/src/'+n for n in ['task.rs','clock_origin_tests.rs','injection_stop_tests.rs','tracer.rs']]
tool=Path('/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin/rustfmt')
def rec(p):
 p=Path(p);d=p.read_bytes();return dict(path=str(p),bytes=len(d),mode=stat.S_IMODE(p.stat().st_mode),sha256=hashlib.sha256(d).hexdigest())
before=[]
for rel in paths:
 p=W/'source'/rel;q=W/'author-format-before-3'/rel;q.parent.mkdir(parents=True,exist_ok=True);q.open('xb').write(p.read_bytes());before.append(rec(q))
def limits():
 resource.setrlimit(resource.RLIMIT_CPU,(30,30));resource.setrlimit(resource.RLIMIT_AS,(16*1024**3,16*1024**3));resource.setrlimit(resource.RLIMIT_FSIZE,(16*1024**2,16*1024**2))
argv=[str(tool),'--edition','2024','--config','skip_children=true',*paths]
started=time.monotonic();forced=False
with (W/'AUTHOR-FORMAT-3.stdout').open('xb') as out,(W/'AUTHOR-FORMAT-3.stderr').open('xb') as err:
 p=subprocess.Popen(argv,cwd=W/'source',stdin=subprocess.DEVNULL,stdout=out,stderr=err,preexec_fn=limits,start_new_session=True)
 try:rc=p.wait(timeout=60)
 except subprocess.TimeoutExpired:forced=True;os.killpg(p.pid,signal.SIGKILL);rc=p.wait()
record=dict(argv=argv,tool=rec(tool),cpu_limit_seconds=30,wall_limit_seconds=60,stream_limit_bytes=16*1024**2,returncode=rc,forced=forced,elapsed_seconds=time.monotonic()-started,before=before,after=[rec(W/'source'/r) for r in paths],stdout=rec(W/'AUTHOR-FORMAT-3.stdout'),stderr=rec(W/'AUTHOR-FORMAT-3.stderr'),qualification=False)
(W/'AUTHOR-FORMAT-3.json').open('x').write(json.dumps(record,indent=2)+'\n')
if forced or rc!=0:raise RuntimeError('author format refused')
print(json.dumps(dict(returncode=rc,elapsed_seconds=record['elapsed_seconds'],qualification=False)))
