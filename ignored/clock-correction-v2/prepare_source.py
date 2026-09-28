#!/usr/bin/python3
"""Materialize exact Git objects plus the preserved clock patch; no SCM writes."""
import hashlib,json,os,shutil,stat,subprocess
from pathlib import Path
W=Path(__file__).resolve().parent
L=W.parent.parent
C=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/kvm-clock-origin-correction-source-v1')
BASE='3d4a401ed8959befad0b2f10db59093725f27432'
TREE='73024912837cbbeaa70b6d2173f5b596eebedcb3'
def require(p,m):
    if not p: raise RuntimeError(m)
def git(*args):return subprocess.check_output(['git','--no-optional-locks',*args],cwd=L)
def rec(p):
    p=Path(p);d=p.read_bytes();return dict(path=str(p),bytes=len(d),mode=stat.S_IMODE(p.stat().st_mode),sha256=hashlib.sha256(d).hexdigest())
def write(n,v):(W/n).open('x').write(json.dumps(v,indent=2)+'\n')
require(git('rev-parse',BASE+'^{tree}').decode().strip()==TREE,'base tree differs')
root=W/'base';root.mkdir(); rows=[]
for r in git('ls-tree','-rz',BASE).split(b'\0'):
    if not r:continue
    meta,name=r.split(b'\t');mode,kind,oid=meta.decode().split();rel=name.decode();p=root/rel;p.parent.mkdir(parents=True,exist_ok=True)
    if kind=='commit':p.mkdir(exist_ok=True);rows.append(dict(path=rel,mode=mode,type='gitlink',commit=oid));continue
    d=git('cat-file','blob',oid)
    if mode=='120000':os.symlink(d.decode(),p);rows.append(dict(path=rel,mode=mode,type='symlink',blob=oid,target=d.decode()))
    else:
        p.write_bytes(d);p.chmod(0o755 if mode=='100755' else 0o644);rows.append(dict(path=rel,mode=mode,type='file',blob=oid,sha256=hashlib.sha256(d).hexdigest()))
source=W/'source';shutil.copytree(root,source,symlinks=True)
patch=C/'SOURCE.patch';require(rec(patch)['sha256']=='f99e50b83981f3c42363bb7fc4968aa05939629741fd728b2abb4f0e6bf7b3ee','clock patch differs')
result=subprocess.run(['/usr/bin/patch','--batch','--fuzz=0','-p1','-i',str(patch)],cwd=source,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
(W/'CLOCK-PATCH.stdout').write_bytes(result.stdout);(W/'CLOCK-PATCH.stderr').write_bytes(result.stderr)
require(result.returncode==0 and b'offset' not in result.stdout and b'fuzz' not in result.stdout,'clock patch application differs')
changed=['reverie-ptrace/src/'+n for n in ['perf.rs','timer.rs','task.rs','tracer.rs','clock_origin_tests.rs']]
for rel in changed:require((source/rel).read_bytes()==(C/'source'/rel).read_bytes(),'clock component differs '+rel)
lock=C/'source/Cargo.lock';shutil.copy2(lock,source/'Cargo.lock')
shutil.copytree(source,W/'before',symlinks=True)
write('BASE-MANIFEST.json',dict(base=BASE,tree=TREE,entries=rows,gitlinks_unexpanded=True))
write('SOURCE-ORIGIN.json',dict(base=BASE,tree=TREE,base_manifest=rec(W/'BASE-MANIFEST.json'),clock_patch=rec(patch),patch_stdout=rec(W/'CLOCK-PATCH.stdout'),patch_stderr=rec(W/'CLOCK-PATCH.stderr'),patch_returncode=result.returncode,clock_paths_equal_frozen_C=changed,lock_origin=rec(lock),lock_copy=rec(source/'Cargo.lock'),tracked_base_exact=True,base_submodule_gitlinks_preserved=True,source_only=True,no_live_scm_write=True,missing_product_AGENTS='L/AGENTS.md absent; initial direct cat failed and is recorded here. Parent AGENTS applies; historical .skill_reset_20260816/AGENTS.md is outside edited paths.'))
print(json.dumps(dict(source=str(source),base=BASE,tree=TREE,entries=len(rows),clock_paths=changed)))
