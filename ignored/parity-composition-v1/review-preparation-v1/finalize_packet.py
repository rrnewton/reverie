from pathlib import Path
import collections,hashlib,json,os,re,shutil,stat,subprocess,sys
P=Path(__file__).resolve().parent;B=P.parent;L=B.parents[1];SRC=B/'source';Q=B/'qualification-v2';QR=B/'qualification-result-v2';F=P/'final-v1'
S=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916');R=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918')
BASE='6e3915b70a71657a08f0028b3f70d6074116206a';MID='0d1b4999b5af370f8a7a13f3d98c99d1ac55c091';HEAD='3d4a401ed8959befad0b2f10db59093725f27432';TREE='73024912837cbbeaa70b6d2173f5b596eebedcb3'
assert sys.flags.optimize==0

def rec(p):
 p=Path(p);s=p.lstat();assert stat.S_ISREG(s.st_mode) and not p.is_symlink(),p
 h=hashlib.sha256()
 with p.open('rb')as f:
  for chunk in iter(lambda:f.read(1024**2),b''):h.update(chunk)
 return dict(path=str(p),bytes=s.st_size,sha256=h.hexdigest(),mode=stat.S_IMODE(s.st_mode))
def put(p,v):
 p=Path(p);p.parent.mkdir(parents=True,exist_ok=True)
 with p.open('x')as f:f.write(v if isinstance(v,str)else json.dumps(v,indent=2)+'\n')
def check(row):
 p=Path(row['path'])
 if row.get('kind')=='symlink':
  assert p.is_symlink()and os.readlink(p)==row['target'];return None
 a=rec(p);assert all(a[k]==v for k,v in row.items()if k in a),(row,a);return a
def git(*args):
 r=subprocess.run(['/usr/bin/git','-C',str(L),*args],capture_output=True,env={**os.environ,'GIT_OPTIONAL_LOCKS':'0','GIT_NO_LAZY_FETCH':'1'},timeout=30);assert r.returncode==0,(args,r.stderr);return r.stdout
assert not F.exists();F.mkdir();C=F/'common';C.mkdir()
assert git('rev-parse',HEAD+'^{tree}').decode().strip()==TREE
assert git('rev-parse',HEAD+'^').decode().strip()==MID and git('rev-parse',MID+'^').decode().strip()==BASE
expected=json.loads((P/'common/SOURCE-AUTHENTICATION.json').read_text())['changed_paths'];assert git('diff','--name-only',BASE,HEAD).decode().splitlines()==expected
manifest=json.loads((B/'SOURCE-MANIFEST.json').read_text());trees={}
for rev in [BASE,HEAD]:
 rows={}
 for entry in git('ls-tree','-rz',rev).split(b'\0'):
  if not entry:continue
  meta,path=entry.split(b'\t');mode,kind,oid=meta.decode().split();rows[path.decode()]=dict(mode=mode,kind=kind,oid=oid)
 trees[rev]=rows
extra=[];checked=[]
for row in manifest:
 rel=row['path'];p=SRC/rel
 if rel not in trees[HEAD]:extra.append(row);continue
 actual=trees[HEAD][rel];assert actual['mode']==row['mode']
 if row['mode']=='160000':assert actual==trees[BASE][rel];checked.append(dict(path=rel,gitlink=actual));continue
 data=os.fsencode(os.readlink(p))if p.is_symlink()else p.read_bytes()
 assert hashlib.sha256(data).hexdigest()==row['sha256']
 oid=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest();assert oid==actual['oid']
 if row['mode']!='120000':assert bool(p.stat().st_mode&0o111)==(row['mode']=='100755')
 checked.append(dict(path=rel,mode=row['mode'],sha256=row['sha256'],git_blob=oid))
assert [r['path']for r in extra]==['Cargo.lock'];assert len(checked)==2630 and len(manifest)==2631
assert set(trees[HEAD])=={r['path']for r in checked}
rootproof=B/'COMMITTED-SOURCE-PROOF-v2.json';assert rootproof.exists()
put(C/'COMMITTED-BINDING.json',dict(base=BASE,cpuid_commit=MID,head=HEAD,tree=TREE,source_manifest=rec(B/'SOURCE-MANIFEST.json'),root_proof=rec(rootproof),verified_tracked_entries=len(checked),extra_qualification_input=rec(SRC/'Cargo.lock'),extra_scope='Cargo.lock is ignored at base/head; actual qualified input, not an omitted tracked change',changed_paths=expected,checked_entries=checked,source_approval=False))
put(C/'COMMITTED.patch',git('diff','--binary',BASE,HEAD).decode())
# Preserve caller preparation history and bind exact architecture sources, not any reviewer's findings.
prereq=json.loads((R/'ignored/cpuid-recovery-source-v1/PREREQUISITES.json').read_text())['records'];primary=[x for x in prereq if '/kvm-cpuid-transport-research-v1/'in x['path']]
for r in primary:check(r)
kr=S/'ignored/kvm-cpuid-transport-research-v1'
ranges={'arch__x86__kvm__cpuid.c':[(2160,2177)],'arch__x86__kvm__cpuid.h':[(184,193)],'arch__x86__kvm__x86.c':[(1028,1037),(1698,1719),(4255,4273),(4614,4631)],'Documentation__virt__kvm__api.rst':[(218,261),(643,704),(8112,8121)]}
ktext='Exact upstream Linux 199c9959d3a9b53f346c221757fc7ac507fbac50; distinct from measured vendor kernel. Complete relevant functions/API sections and explicit switch-case context; complete source paths available for follow-up.\n\n'
for name,rs in ranges.items():
 p=kr/name;ls=p.read_text().splitlines(keepends=True)
 for lo,hi in rs:ktext+=f'SOURCE {p} SHA256 {rec(p)["sha256"]} lines {lo}..{hi}\n'+''.join(ls[lo-1:hi])+'\n'
put(C/'KERNEL-CPUID-CONTEXT.txt',ktext);put(C/'KERNEL-INPUTS.json',dict(revision='199c9959d3a9b53f346c221757fc7ac507fbac50',records=[check(x)for x in primary],read_scope=ranges))
# Complete additional unchanged timestamp call reached from the new shared dispatcher.
vm=SRC/'reverie-kvm/src/vm.rs';ls=vm.read_text().splitlines(keepends=True)
start=next(i for i,l in enumerate(ls)if 'pub(crate) fn timestamp_counter_exception('in l);end=next(i+1 for i in range(start+1,len(ls))if ls[i].rstrip()=='    }')
put(C/'TIMESTAMP-CALLER.txt',f'SOURCE {vm} SHA256 {rec(vm)["sha256"]} lines {start+1}..{end}\n'+''.join(ls[start:end]))
# Read raw qualification results without repeating or modifying any execution.
results=json.loads((QR/'RESULTS.json').read_text());assert rec(QR/'RESULTS.json')['sha256']=='a98dab4c5a5e86b45730de06b328a8cc97da5766d949dc103dc6aaca2ecba66f'
assert results['all_qualified']and results['attempted']==results['accepted']==92 and results['passed_declarations']==83 and results['failed_declarations']==results['ignored_declarations']==0
result_inputs=json.loads((QR/'INPUTS.json').read_text())['records'];qualified_files=[];qualified_links=[]
for row in result_inputs:
 actual=check(row)
 if actual:qualified_files.append(actual)
 else:qualified_links.append(dict(path=row['path'],target=row['target']))
order=json.loads((B/'RUN-ORDER.json').read_text())['phases'];assert [r['name']for r in results['phases']]==order
groups=json.loads((Q/'SELECTORS.json').read_text())['groups'];summary=[];rawtext='Exact retained raw test stdout/stderr, unchanged. Header names and hashes identify each original stream. No shell filtering or rerun.\n\n';rawrefs=[]
for phase in results['phases']:
 name=phase['name'];plan=json.loads(Path(phase['plan']['path']).read_text());receipt=json.loads(Path(phase['result']['path']).read_text());observer=json.loads(Path(phase['observer']['path']).read_text())
 for key in ['plan','result','observer']:check(phase[key])
 assert receipt['plan_sha256']==phase['plan']['sha256']and receipt['raw_status']==0 and receipt['accepted'] and receipt['terminal_authenticated']and receipt['inputs_unchanged']
 assert observer['accounting_complete']and observer['final_accounting']['cgroup_empty']
 limit=plan['limits'];large=plan['kind']in['metadata','compile','check'];assert limit['aggregate_cpu_usec']==(600000000if large else 30000000)and limit['wall_seconds']==(900if large else 60)
 assert limit['memory_bytes']==17179869184 and limit['swap_bytes']==0 and limit['free_floor_bytes']==107374182400
 assert plan['environment']['REVERIE_REQUIRE_KVM']=='1'and plan['environment']['CARGO_BUILD_JOBS']=='2'
 row=dict(phase=name,kind=plan['kind'],raw=0,accepted=True,cpu_seconds=phase['cpu_seconds'],payload_seconds=phase['payload_seconds'])
 if name in groups:
  streams={}
  for stream in ['stdout','stderr']:
   path=Q/'observer'/name/stream;text=path.read_text();streams[stream]=text;r=rec(path);rawrefs.append(r)
   rawtext+=f'PHASE {name} SELECTOR {groups[name]["names"][0]} STREAM {stream} PATH {path} SHA256 {r["sha256"]}\n'+text+('\n'if not text.endswith('\n')else'')+'END STREAM\n\n'
  events=[json.loads(x)for x in streams['stdout'].splitlines()]
  done=[x for x in events if x.get('type')=='test'and x.get('event')in['ok','failed','ignored']];assert len(done)==1 and done[0]['event']=='ok'and done[0]['name']==groups[name]['names'][0]
  suites=[x for x in events if x.get('type')=='suite'and x.get('event')in['ok','failed']];assert len(suites)==1 and suites[0]['passed']==1 and suites[0]['failed']==suites[0]['ignored']==0
  assert not re.search(r'\bskip(?:ped|ping)?\b|\bunexecuted\b|KVM unavailable|cannot open /dev/kvm',streams['stdout']+'\n'+streams['stderr'],re.I)
  row.update(selector=done[0]['name'],artifact=groups[name]['artifact'],test=done[0])
 summary.append(row)
put(C/'RAW-TEST-OUTPUTS.txt',rawtext)
put(C/'RAW-RESULT-READBACK.json',dict(results=rec(QR/'RESULTS.json'),raw_execution_audit=rec(QR/'RAW-EXECUTION-AUDIT.json'),inputs=rec(QR/'INPUTS.json'),source_readback=rec(QR/'READBACK.json'),phase_count=len(summary),test_count=len(rawrefs)//2,phases=summary,records_authenticated=len(result_inputs),original_refusal_preserved=results['earlier_attempts'],total_including_first_refusal_cpu_unknown=True,no_new_execution=True,no_source_verdict=True))
put(C/'QUALIFICATION.md',f'''The current immutable composition completed 92 accepted phases and 83 passing declarations, zero failed or ignored. Raw structured results and both streams were read for every selected declaration. Counts are 43 library, 34 static ELF, two vmcall and four read-clock declarations; physical fixture modes are separate. Aggregate CPU for the qualified continuation is {results['total_cpu_seconds']:.6f} seconds, summed payload {results['total_payload_seconds']:.6f} seconds, observer transport {results['total_observer_transport_seconds']:.6f} seconds. These are different measurement boundaries, not individual test duration or elapsed whole-review time.

The first metadata attempt remains raw 137, accepted=false, with incomplete accounting after RefUnit timeout. Its exact receipt/observer/proof remain separate. Its CPU is not added as a known value. The later continuation used a new private lease under the original bounds, same source and fresh target; no claim repairs the first receipt. The final qualified source used a snapshot while live L remained at 68569; current committed equality is separately proved for {HEAD}, tree {TREE}, after landing the byte-identical base {BASE}.

Four actual Cargo harnesses and the linked KVM library were newly built (fresh=false). Retained copies preserve exact ELF bytes in distinct inodes. Source, actual lock, dependency/toolchain/loader and raw plans/results are bound in original INPUTS. The source tree has 2,630 tracked entries and one additional ignored Cargo.lock qualification input. No broader parity, Hermit integration, clock-origin or source-review verdict follows from these finite results. The historical README/predecessor author notes predate this run; this current evidence, not their future-tense wording, is authoritative for execution.
''')
probe=R/'ignored/cpuid-transport-probe-v1';probe_docs=[('transport-observations',probe/'final-v1/HARDWARE-OBSERVATIONS.json'),('transport-raw-stdout',probe/'qualification-v1/observer/probe-01/stdout'),('transport-raw-stderr',probe/'qualification-v1/observer/probe-01/stderr')]
probe_bound=[check(r)for r in prereq if '/cpuid-transport-probe-v1/'in r['path']]
additional_docs=[('committed-target',C/'QUALIFICATION.md'),('kernel-cpuid-semantics',C/'KERNEL-CPUID-CONTEXT.txt'),('unchanged-timestamp-caller',C/'TIMESTAMP-CALLER.txt'),('complete-lib-module-registration',SRC/'reverie-kvm/src/lib.rs'),('complete-terminal-admission-tests',SRC/'reverie-kvm/src/terminal_runtime_tests.rs'),('compiled-artifact-binding',Q/'retained-binaries/BINDING.json'),('raw-composed-test-outputs',C/'RAW-TEST-OUTPUTS.txt')]+probe_docs
lanes=[]
for lane in ['claude','native']:
 D=F/lane;D.mkdir();(D/'chunks').mkdir();OLD=P/lane
 helpers=['launch-review.py','audit-final.py']if lane=='claude'else['launch-guard.py','run_stage.py','audit_source.py','audit_split_reads.py']
 for name in helpers:shutil.copyfile(OLD/name,D/name);assert (OLD/name).read_bytes()==(D/name).read_bytes()
 for name in ['GROUNDING_AUTHENTICATION.json','GROUNDING_INPUTS.json','OWN-PRIOR-CUSTODY.json']:shutil.copyfile(OLD/name,D/name)
 prev=json.loads((OLD/'COMPLETE_READS-PREPARED.json').read_text());index=prev if lane=='claude'else[dict(name=d['name'],source=d['document'],lines=d['lines'],chunks=d['chunks'])for d in prev['documents']]
 # Source corpus is unchanged; only append current execution/commit/primary context documents.
 for name,path in additional_docs:
  lines=path.read_text().splitlines(keepends=True);parts=[];lo=0
  while lo<len(lines):
   hi=lo;n=0
   while hi<len(lines)and hi-lo<160 and(n+len(lines[hi].encode())<=9000 or hi==lo):n+=len(lines[hi].encode());hi+=1
   cp=D/'chunks'/f'{name}-{lo+1:05d}-{hi:05d}.txt';put(cp,''.join(lines[lo:hi]));parts.append(dict(**rec(cp),first_line=lo+1,last_line=hi));lo=hi
  index.append(dict(name=name,source=rec(path),lines=len(lines),chunks=parts))
 target=json.loads((OLD/'TARGET.template.json').read_text());target.update(status='FROZEN PREPARATION; ROOT MUST AUTHORIZE LAUNCH',head=HEAD,tree=TREE,parent=MID,qualification=rec(QR/'READBACK.json'),committed_binding=rec(C/'COMMITTED-BINDING.json'),mandatory_documents=len(index),mandatory_chunks=sum(len(x['chunks'])for x in index),mandatory_lines=sum(x['lines']for x in index))
 records={r['path']:r for r in json.loads((OLD/'inputs-PREPARED.json').read_text())['files']};links={r['path']:r for r in json.loads((OLD/'inputs-PREPARED.json').read_text())['symlinks']}
 for r in qualified_files+probe_bound+primary:
  r=check(r);records[r['path']]=r
 for r in qualified_links:links[r['path']]=r
 # A mutable target copy may occur as historical filename text in old receipts, but only immutable record paths enter this set.
 def add(path):r=rec(path);records[r['path']]=r
 for f in [rootproof,*QR.iterdir(),B/'CPUID-ON-V7-READBACK.json',B/'COMPOSITION-READBACK.json',SRC/'Cargo.lock',Path(__file__),P/'PREPARATION-INITIAL-REFUSAL.json',P/'PREPARATION-CORRECTION.patch',P/'prepare_packet_initial.py',P/'PLAN-PREPARED.json',P/'PREPARATION-READBACK.json']:
  if f.is_file():add(f)
 for f in C.iterdir():
  if f.is_file():add(f)
 for d in index:
  add(d['source']['path'])
  for c in d['chunks']:add(c['path'])
 target['expected_symlinks']=len(links);put(D/'TARGET.json',target)
 put(D/'COMPLETE_READS.json',index if lane=='claude'else dict(documents=[dict(name=d['name'],document=d['source'],lines=d['lines'],chunks=d['chunks'])for d in index],mandatory_documents=target['mandatory_documents'],mandatory_chunks=target['mandatory_chunks'],mandatory_lines=target['mandatory_lines']))
 put(D/'READ-INDEX.txt','Read every bounded chunk completely in this listed order. Current execution/head facts are appended after source/caller context; historical future-tense preparation notes do not override final results. Your own canonical grounding remains separately authenticated. Full original paths and identities are in COMPLETE_READS.json and common/COMMITTED-BINDING.json.\n\n'+'\n'.join(f'{i+1}. {d["name"]}: {d["source"]["path"]} SHA256 {d["source"]["sha256"]}\n'+''.join('   '+c['path']+'\n'for c in d['chunks'])for i,d in enumerate(index)))
 prompt=(OLD/'prompt.template.txt').read_text().replace('@@HEAD@@',HEAD).replace('@@TREE@@',TREE).replace('@@FINAL_EVIDENCE@@',str(C/'RAW-RESULT-READBACK.json')).replace('@@INDEX@@',str(D/'READ-INDEX.txt'))
 assert '@@'not in prompt
 prompt+='\nCommitted-source evidence is '+str(C/'COMMITTED-BINDING.json')+'; full original source remains bound. The complete upstream kernel functions/cases/API sections and distinct measured vendor-kernel transport values are appended to the mandatory index. Those five old probe phases are prerequisites, not extra composed test declarations. No disputed prefix/TF/RF native semantics is inferred from an MSR transport probe.\n'
 put(D/('prompt.txt'if lane=='claude'else'01-source.prompt.txt'),prompt)
 for f in D.iterdir():
  if f.is_file():add(f)
 for r in records.values():check(r)
 for r in links.values():assert Path(r['path']).is_symlink()and os.readlink(r['path'])==r['target']
 put(D/'inputs.json',dict(files=sorted(records.values(),key=lambda r:r['path']),symlinks=sorted(links.values(),key=lambda r:r['path'])))
 lanes.append(dict(lane=lane,session=target['session_id'],target=rec(D/'TARGET.json'),prompt=rec(D/('prompt.txt'if lane=='claude'else'01-source.prompt.txt')),index=rec(D/'COMPLETE_READS.json'),inputs=rec(D/'inputs.json'),files=len(records),aliases=len(links),mandatory_documents=target['mandatory_documents'],mandatory_chunks=target['mandatory_chunks'],mandatory_lines=target['mandatory_lines'],cwd=str(D),command=['with-proxy','env','PYTHONOPTIMIZE=0','/usr/bin/python3','-B',str(D/('launch-review.py'if lane=='claude'else'launch-guard.py'))]))
plan=dict(status='PREPARATION ONLY; ROOT INSPECTION/AUTHORIZATION REQUIRED',base=BASE,head=HEAD,tree=TREE,lanes=lanes,bounds=dict(wall_seconds=1800,claude_per_output_file_bytes=67108864,native_combined_raw_bytes=67108864,all_uid_cli_cap=15,model_override=False),order='First actual launch, verify effective session/model/policy initialization, then fresh all-UID census by the unchanged second launcher. No launch has occurred.',audits='Claude: unchanged acceptance predicates in audit-final.py (report scope sentence only corrected); native: unchanged audit_source.py final then audit_split_reads.py final as necessary. Full reports/raw errors/source custody/order/completeness must be collected before any verdict credit.',qualification=rec(QR/'READBACK.json'),source_binding=rec(C/'COMMITTED-BINDING.json'),no_source_approval=True)
put(F/'PLAN.json',plan)
put(F/'REPORT.md',f'''Prepared independent continuations for the complete linear Reverie successor {BASE}..{HEAD}, tree {TREE}. The head contains the CPUID commit {MID} followed by capture identity. All 2,630 committed entries match the qualified snapshot; its extra Cargo.lock is ignored and retained solely as a qualification input. Both linear patches and the complete committed diff are bound. The current run records 92 accepted phases / 83 passing unique declarations, with the first raw 137 metadata refusal preserved separately. No reviewer has launched or approved this composition.

Each lane receives only its own authenticated canonical grounding and timestamp report/custody, plus complete new source and current evidence. All thirteen changed files are fully bound; mandatory reads include complete diff, all new modules/tests, complete changed functions and selected lifecycle/caller context, exact current raw test streams, artifact binding and caller/observer. Huge unchanged source files remain available for follow-up without being falsely described as read in full. Source coverage is not approval. The kernel corpus is exact upstream source; the transport probe is distinct vendor-kernel observation and not added to composed declaration counts.

Actual commands, bounds and targets are in PLAN.json. Launch helpers are byte-identical to the inspected predecessors; Claude audit only changes its scope sentence (delta retained in ../claude/AUDIT-WORDING-DELTA.patch). Whole with-proxy invocation, no model override, fresh all-UID capacity check, sequential admission and actual effective-policy verification remain required. The frozen final source and result packet now fill both former placeholders. Root inspection and explicit launch authorization remain outstanding.

Preparation itself once stopped at an absent optional selector purpose field; that raw exception and original script are retained. Only packet construction was corrected to represent the absent description as null; selector names, assertions, qualification callers, source and budgets were untouched. All outputs remain beneath the authorized review-preparation-v1 directory.
''')
put(F/'SETUP.json',dict(plan=rec(F/'PLAN.json'),report=rec(F/'REPORT.md'),script=rec(__file__),source_binding=rec(C/'COMMITTED-BINDING.json'),result_readback=rec(C/'RAW-RESULT-READBACK.json'),lanes=lanes,reviewers_launched=0,product_scm_cache_lease_mutated=False))
print(json.dumps(dict(setup=rec(F/'SETUP.json'),plan=rec(F/'PLAN.json'),lanes=[{k:v for k,v in x.items()if k in ['lane','files','aliases','mandatory_documents','mandatory_chunks','mandatory_lines']}for x in lanes]),indent=2))
