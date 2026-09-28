#!/usr/bin/python3
"""Freeze authored source and complete diffs without build or SCM mutation."""
import difflib,hashlib,json,os,stat,subprocess
from pathlib import Path
W=Path(__file__).resolve().parent;L=W.parent.parent;R=W/'source';B=W/'base';P=W/'before'
def require(p,m):
 if not p:raise RuntimeError(m)
def rec(p):
 p=Path(p);d=p.read_bytes();return dict(path=str(p),bytes=len(d),mode=stat.S_IMODE(p.stat().st_mode),sha256=hashlib.sha256(d).hexdigest())
def write(n,v):(W/n).open('x').write(json.dumps(v,indent=2)+'\n')
def manifest(root):
 out=[]
 for p in sorted(root.rglob('*')):
  if p.is_symlink():out.append(dict(path=str(p.relative_to(root)),mode='120000',sha256=hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest()))
  elif p.is_file():out.append(dict(path=str(p.relative_to(root)),mode='100755' if p.stat().st_mode&0o111 else '100644',sha256=rec(p)['sha256']))
 return out
base={x['path']:x for x in manifest(B)};before={x['path']:x for x in manifest(P)};after={x['path']:x for x in manifest(R)}
changed=sorted(k for k in base.keys()|after.keys() if base.get(k)!=after.get(k) and k!='Cargo.lock')
delta=sorted(k for k in before.keys()|after.keys() if before.get(k)!=after.get(k))
require(changed==sorted('reverie-ptrace/src/'+n for n in ['perf.rs','timer.rs','task.rs','tracer.rs','clock_origin_tests.rs','injection_stop_tests.rs']),'unexpected source scope')
require(delta==sorted('reverie-ptrace/src/'+n for n in ['task.rs','tracer.rs','clock_origin_tests.rs','injection_stop_tests.rs']),'unexpected successor delta')
def patch(a,b,names):
 out=[]
 for rel in names:
  old=a/rel;new=b/rel;ob=old.read_text().splitlines(True) if old.exists() else [];nb=new.read_text().splitlines(True)
  out.append(f'diff --git a/{rel} b/{rel}\n')
  if not old.exists():out.append('new file mode 100644\n')
  out.extend(difflib.unified_diff(ob,nb,fromfile='a/'+rel if old.exists() else '/dev/null',tofile='b/'+rel))
 return ''.join(out)
(W/'SOURCE.patch').open('x').write(patch(B,R,changed))
(W/'INJECTION-CORRECTION.patch').open('x').write(patch(P,R,delta))
(W/'ORACLE-CHANGES.patch').open('x').write(patch(P,R,['reverie-ptrace/src/clock_origin_tests.rs']))
write('SOURCE-MANIFEST.json',list(after.values()))
write('CONTINUITY.json',dict(base='3d4a401ed8959befad0b2f10db59093725f27432',tree='73024912837cbbeaa70b6d2173f5b596eebedcb3',full_changed_paths=changed,successor_changed_paths=delta,other_base_entries_identical=len(base)-len([p for p in changed if p in base]),dependency_manifests_unchanged=all(base.get(k)==v for k,v in after.items() if k.endswith('Cargo.toml') or k=='rust-toolchain.toml'),same_lock_as_prior=rec(R/'Cargo.lock')['sha256']==rec(P/'Cargo.lock')['sha256'],timestamp_cpuid_capture_source_unchanged=True,no_scm_or_product_edits=True))
state={}
for name,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
 d=subprocess.check_output(['/usr/bin/git','--no-optional-locks','-C',str(L),*args]);state[name]=hashlib.sha256(d).hexdigest() if name=='index' else d.decode().strip()
require(state['head']=='3d4a401ed8959befad0b2f10db59093725f27432','live head differs')
write('OWNER-SCM.json',dict(state=state,read_only=True,tracked_status=subprocess.check_output(['/usr/bin/git','--no-optional-locks','-C',str(L),'status','--porcelain=v1','--untracked-files=no']).decode()))
write('PREPARATION-LIMITS.json',dict(product_execution=False,author_formats=[rec(W/('AUTHOR-FORMAT'+suffix+'.json')) for suffix in ['', '-2','-3']],read_errors=['Initial L/AGENTS.md direct read: absent; parent instructions apply.','Exploratory reverie-process/src/command.rs search: nonexistent; corrected to builder.rs.'],edit_refusals=['One apply_patch expected an unformatted one-line clock assertion after rustfmt had split it; refused without mutation, then exact formatted line corrected.'],author_review_not_independent_approval=True))
print(json.dumps(dict(changed_paths=changed,successor_paths=delta,source_patch=rec(W/'SOURCE.patch'),delta=rec(W/'INJECTION-CORRECTION.patch'),manifest=rec(W/'SOURCE-MANIFEST.json'))))
