from pathlib import Path
import difflib,hashlib,json,os,re,shutil,subprocess
R=Path(__file__).resolve().parents[2];D=Path(__file__).resolve().parent;F=R/'ignored/timer-integration-review-response-20260918/frozen-v2';Q=D/'positive-v2';O=D/'frozen-v3';O.mkdir()
def rec(p):
 p=Path(p);b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest())
def write(p,v):p.parent.mkdir(parents=True,exist_ok=True);p.write_text(json.dumps(v,indent=2)+'\n')
base=subprocess.check_output(['git','rev-parse','HEAD'],cwd=R,timeout=30).decode().strip();assert base=='f97b7be1de4e2ef10ecc24cee5d8cc47f2fd254f'
paths=subprocess.check_output(['git','diff','HEAD','--name-only'],cwd=R,timeout=30).decode().splitlines()+subprocess.check_output(['git','ls-files','--others','--exclude-standard','--','reverie/src','reverie-kvm/src','reverie-kvm/tests'],cwd=R,timeout=30).decode().splitlines();paths=sorted(set(paths));assert len(paths)==17
manifest=json.loads((Q/'source-manifest.json').read_text());byname={r['path']:r for r in manifest};inputs=[];patches={'SOURCE':[],'DELTA':[]};changed=[]
def patch(name,b,a):
 if b==a:return ''
 h='diff --git a/'+name+' b/'+name+'\n'
 if b is None:h+='new file mode 100644\n'
 return h+''.join(difflib.unified_diff([] if b is None else b.decode().splitlines(True),a.decode().splitlines(True),fromfile='/dev/null' if b is None else 'a/'+name,tofile='b/'+name))
for name in paths:
 a=(R/name).read_bytes();assert hashlib.sha256(a).hexdigest()==byname[name]['sha256']
 p=subprocess.run(['git','show',base+':'+name],cwd=R,capture_output=True,timeout=30);b=p.stdout if p.returncode==0 else None
 priorpath=F/'source/after'/name
 if priorpath.exists():prior=priorpath.read_bytes()
 else:prior=b
 row=dict(path=name,before_exists=b is not None,delta_changed=prior!=a)
 for category,raw in [('before',b),('v2',prior),('after',a)]:
  if raw is None:row[category]=None;continue
  target=O/'source'/category/name;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(raw);row[category]=rec(target)
 patches['SOURCE'].append(patch(name,b,a));patches['DELTA'].append(patch(name,prior,a));changed.append(row)
for name,parts in patches.items():(O/(name+'.patch')).write_text(''.join(parts))
# Whole current context, copied from frozen predecessor scope plus concrete cancellation/admission paths.
context=[p.relative_to(F/'context').as_posix()for p in(F/'context').rglob('*')if p.is_file()]
context+=['reverie-kvm/src/runtime/failure_tests.rs','reverie-kvm/src/runtime.rs','reverie-kvm/src/parked_signal_runtime.rs','reverie-kvm/src/signal_dequeue_tests.rs','reverie-kvm/tests/fixtures/parked_signal.c','Cargo.lock']
for name in sorted(set(context)-set(paths)):
 p=R/name;out=O/'context'/name;out.parent.mkdir(parents=True,exist_ok=True);out.write_bytes(p.read_bytes());inputs.append(dict(role='unchanged-context',repository_path=name,record=rec(out),live=rec(p)))
shutil.copyfile(Q/'source-manifest.json',O/'source-manifest.json')
# Verify both patch paths by applying only in owned disposable ignored directories, no Git index/refs.
recon=[]
for kind,start in [('SOURCE','before'),('DELTA','v2')]:
 temp=D/('reconstruct-v3-'+kind.lower());temp.mkdir()
 for row in changed:
  if row[start]:
   dest=temp/row['path'];dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(row[start]['path'],dest)
 env=dict(os.environ,GIT_CEILING_DIRECTORIES=str(D),GIT_NO_LAZY_FETCH='1',GIT_OPTIONAL_LOCKS='0')
 for check in [True,False]:
  args=['git','apply']+(['--check']if check else[])+[str(O/(kind+'.patch'))];p=subprocess.run(args,cwd=temp,env=env,capture_output=True,timeout=30);assert p.returncode==0,p.stderr
 for row in changed:assert(temp/row['path']).read_bytes()==Path(row['after']['path']).read_bytes()
 recon.append(dict(patch=rec(O/(kind+'.patch')),before=start,check_returncode=0,apply_returncode=0,after_paths_verified=len(changed),destination=str(temp)))
write(O/'SOURCE_INPUTS.json',dict(base=base,base_tree=subprocess.check_output(['git','rev-parse',base+'^{tree}'],cwd=R,timeout=30).decode().strip(),branch=subprocess.check_output(['git','branch','--show-current'],cwd=R,timeout=30).decode().strip(),full_source_manifest=rec(O/'source-manifest.json'),changed=changed,context=inputs,prior_frozen_patch=rec(F/'SOURCE.patch'),prior_frozen_readback=rec(F/'READBACK.json'),own_grounding=rec(D/'GROUNDING.json'),review=rec(Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored/kvm-timer-reverie-claude-review-v2-20260918/CLAUDE-REPORT.md')),scope='Uncommitted sole-writer correction; no independent approval or Hermit integration claim.'))
api_paths=['reverie/src/guest.rs','reverie/src/lib.rs','reverie/src/signal.rs','reverie/src/signal_observation.rs','reverie/src/tool.rs','reverie-kvm/src/error.rs'];api=[]
for name in api_paths:
 lines=(R/name).read_text().splitlines();decls=[dict(line=i,text=line.strip())for i,line in enumerate(lines,1)if re.match(r'\s*(pub (?:enum|struct|trait|type|use|fn)|(?:async )?fn (?:observe_signal_dequeues|handle_signal_dequeue|signal_task_identity|parked_signal_site|signal_observation_lease|observe_parked_signal|terminate_from_parked_signal|parked_signal_failure_context|cancel_parked_signal|queue_process_alarm_signal))',line)]
 api.append(dict(path=name,source=rec(O/'source/after'/name),declarations=decls))
write(O/'API_INVENTORY.json',dict(files=api,delta='No Guest or Tool method/type signature changes from frozen-v2. One public KVM Error variant SignalObservationRequiresToolThreads adds explicit setup refusal for statically incompatible opt-in Host ownership. Tool/Caught/ack docs clarify existing contracts. Private failure channel and terminal cleanup preserve raw guest syscall APIs.',hermit_scope='sole_signal_receiver remains; separate single-threaded processes retain existing per-tgid behavior. Deterministic selection among sibling receivers and arbitrary parked request continuation need a separate reviewed extension. FIFO sequence alone proves neither deterministic recipient nor deadlock-free scheduler policy.'))
write(O/'RECONSTRUCTION.json',recon)
print(json.dumps(dict(paths=len(paths),delta_paths=sum(x['delta_changed']for x in changed),source=rec(O/'SOURCE.patch'),delta=rec(O/'DELTA.patch'),inputs=rec(O/'SOURCE_INPUTS.json')),indent=2))
