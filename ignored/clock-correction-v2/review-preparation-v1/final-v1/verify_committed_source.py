"""Read-only equality of exact committed tree and the actual qualified snapshot."""
from pathlib import Path
import hashlib
import json
import os
import stat
import subprocess
import sys

F=Path(__file__).resolve().parent
W=F.parent.parent
L=W.parent.parent
SOURCE=W/'v2-fix/source'
HEAD='a9449f8ee22a17ba87a106c2af957a938fa57c45'
BASE='b5e2ab49cd99e5d456fa0238b8cebd75958c529f'
TREE='da3e0a5c13bbd1b95adca1bc3ac0037b89494544'

def req(value,message):
    if not value:raise RuntimeError(message)

def git(*args,input=None,ok=(0,)):
    env=dict(os.environ,GIT_OPTIONAL_LOCKS='0')
    result=subprocess.run(['git','-C',str(L),*args],input=input,capture_output=True,env=env)
    req(result.returncode in ok,(args,result.returncode,result.stderr.decode(errors='replace')))
    return result

def rec(path):
    p=Path(path);req(p.is_file() and not p.is_symlink(),str(p));h=hashlib.sha256()
    with p.open('rb') as stream:
        for b in iter(lambda:stream.read(1024*1024),b''):h.update(b)
    return dict(path=str(p),bytes=p.stat().st_size,sha256=h.hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))

req(sys.flags.optimize==0,'PYTHONOPTIMIZE=0 is required')
req(git('rev-parse','HEAD').stdout.decode().strip()==HEAD,'live head drift')
req(git('rev-parse',HEAD+'^{tree}').stdout.decode().strip()==TREE,'tree drift')
req(git('rev-list','--parents','-n','1',HEAD).stdout.decode().split()==[HEAD,BASE],'not sole exact parent')
require_paths=['reverie-ptrace/src/'+name for name in ['clock_origin_tests.rs','injection_stop_tests.rs','perf.rs','task.rs','timer.rs','tracer.rs']]
changed=git('diff','--name-only',BASE,HEAD).stdout.decode().splitlines()
req(changed==require_paths,'changed population differs')
req(not git('status','--porcelain','--untracked-files=no').stdout,'tracked live source dirty')

def tree(commit):
    rows=[]
    for line in git('ls-tree','-rz',commit).stdout.split(b'\0'):
        if not line:continue
        metadata,path=line.split(b'\t',1);mode,kind,oid=metadata.decode().split()
        rows.append(dict(path=path.decode(),mode=mode,kind=kind,oid=oid))
    return rows

rows=tree(HEAD);old={r['path']:r for r in tree(BASE)}
objects=sorted({r['oid'] for r in rows if r['kind']=='blob'})
raw=git('cat-file','--batch',input=('\n'.join(objects)+'\n').encode()).stdout
position=0;blobs={}
for wanted in objects:
    end=raw.index(b'\n',position);oid,kind,size=raw[position:end].decode().split();size=int(size)
    start=end+1;data=raw[start:start+size]
    req(oid==wanted and kind=='blob' and len(data)==size and raw[start+size:start+size+1]==b'\n','invalid Git object framing')
    blobs[oid]=data;position=start+size+1
req(position==len(raw),'extra object bytes')

manifest=json.loads((W/'v2-fix/SOURCE-MANIFEST.json').read_text())
bound={r['path']:r for r in manifest};checked=[];regular=aliases=gitlinks=0
for row in rows:
    path=SOURCE/row['path'];mode=row['mode']
    if row['kind']=='commit':
        req(row==old[row['path']],'gitlink changed from landed base')
        req(row['path'] not in bound and path.is_dir() and not path.is_symlink() and not any(path.iterdir()),'gitlink not exact empty carrier')
        checked.append(dict(**row,qualified_carrier=str(path),empty=True));gitlinks+=1;continue
    req(row['path'] in bound,'missing qualified entry')
    expected=bound[row['path']];data=blobs[row['oid']];sha=hashlib.sha256(data).hexdigest()
    req(expected['mode']==mode and expected['sha256']==sha,'manifest/Git mismatch: '+row['path'])
    if mode=='120000':
        req(path.is_symlink(),'missing actual alias')
        target=os.readlink(path);req(target.encode()==data,'alias target mismatch')
        checked.append(dict(**row,qualified_path=str(path),sha256=sha,target=target,target_bytes=len(data)));aliases+=1
    else:
        req(mode in ('100644','100755'),'unexpected Git mode')
        actual=rec(path);req(actual['sha256']==sha and actual['bytes']==len(data) and actual['mode']==int(mode[-3:],8),'qualified bytes/mode mismatch')
        checked.append(dict(**row,qualified_file=actual));regular+=1

extras=sorted(set(bound)-{r['path'] for r in rows})
req(extras==['Cargo.lock'],'unexpected qualification-only source')
lock=rec(SOURCE/'Cargo.lock');req(lock['sha256']==bound['Cargo.lock']['sha256'],'lock source drift')
req(git('check-ignore','--no-index','Cargo.lock').stdout.decode().strip()=='Cargo.lock','extra lock not ignored')
req(lock['sha256']=='1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856','lock provenance drift')
patch=F/'COMMITTED-SOURCE.patch'
with patch.open('xb') as stream:stream.write(git('diff','--binary',BASE,HEAD,'--',*require_paths).stdout)
proof=dict(repository=str(L),base=BASE,head=HEAD,tree=TREE,sole_parent=BASE,python_optimize=sys.flags.optimize,optional_git_locks='0',tracked_checkout_clean=True,changed_paths=changed,tracked_entries=len(rows),regular_files=regular,literal_aliases=aliases,unchanged_gitlinks=gitlinks,qualified_manifest=rec(W/'v2-fix/SOURCE-MANIFEST.json'),qualified_source_target=rec(W/'v2-fix/TARGET.json'),qualified_patch=rec(W/'v2-fix/SOURCE.patch'),actual_committed_patch=rec(patch),all_tracked_bytes_types_modes_equal=True,extra_ignored_qualification_inputs=[lock],entries=checked,scope='Source identity proof only, not source review or runtime approval.')
out=F/'COMMITTED-SOURCE-PROOF.json'
with out.open('x') as stream:json.dump(proof,stream,indent=2);stream.write('\n')
print(json.dumps(dict(proof=rec(out),tracked_entries=len(rows),regular_files=regular,aliases=aliases,gitlinks=gitlinks,ignored_lock=lock),indent=2))
