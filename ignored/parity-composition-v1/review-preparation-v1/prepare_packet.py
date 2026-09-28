from pathlib import Path
import collections,difflib,hashlib,json,os,re,shutil,stat,subprocess,sys
P=Path(__file__).resolve().parent;B=P.parent;L=B.parents[1];SRC=B/'source'
H=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918')
S=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916')
O=H/'ignored/kvm-rdtsc-v7-review-final-v1';F=H/'ignored/kvm-rdtsc-v7-claude-verdict-followup-v1'
BASE='6e3915b70a71657a08f0028b3f70d6074116206a';V7='68569ed7fd5c3667456d1d3e0d74ba66264242f4';BTREE='8e9a1d6b22b421992508e26855a568314a12bddf';HSHA='39ac9f8a55f1b4d17b8c4c0001ce6c4d8a763435'
assert sys.flags.optimize==0

def rec(path):
 p=Path(path);s=p.lstat();assert stat.S_ISREG(s.st_mode) and not p.is_symlink(),p
 h=hashlib.sha256()
 with p.open('rb')as f:
  for b in iter(lambda:f.read(1024**2),b''):h.update(b)
 return dict(path=str(p),bytes=s.st_size,sha256=h.hexdigest(),mode=stat.S_IMODE(s.st_mode))
def check(r):
 a=rec(r['path']);assert all(a[k]==v for k,v in r.items()if k in a),(r,a);return a
def put(p,value):
 p=Path(p);p.parent.mkdir(parents=True,exist_ok=True)
 text=value if isinstance(value,str)else json.dumps(value,indent=2)+'\n'
 if p.exists():
  assert p.read_text()==text,('prepared bytes differ; no overwrite',p);return
 with p.open('x')as f:f.write(text)
def git(*args,repo=L):
 r=subprocess.run(['/usr/bin/git','-C',str(repo),*args],env={**os.environ,'GIT_OPTIONAL_LOCKS':'0','GIT_NO_LAZY_FETCH':'1'},capture_output=True,timeout=30)
 assert r.returncode==0,(args,r.stderr.decode());return r.stdout

def functions(lines):
 found=[]
 for start,line in enumerate(lines):
  m=re.match(r'^(\s*)(?:(?:pub(?:\([^)]*\))?|async|unsafe|const|extern\s+"[^"]+")\s+)*fn\s+(\w+)',line)
  if not m:continue
  indent=m.group(1);name=m.group(2);end=None
  # Ordinary Rust functions here terminate at their declaration indentation.
  if '{'in line and line.rstrip().endswith('}'):end=start+1
  else:
   for j in range(start+1,len(lines)):
    if re.match(r'^'+re.escape(indent)+r'}(?:\s*//.*)?\s*$',lines[j]):end=j+1;break
    if lines[j].strip().endswith(';') and '{'not in ''.join(lines[start:j+1]):end=j+1;break
  if end is None:continue
  lo=start
  while lo>0 and lines[lo-1].strip() and (lines[lo-1].lstrip().startswith(('#[','///','//'))):lo-=1
  found.append(dict(name=name,start=lo,end=end,declaration=start))
 return found

def merge(ranges):
 out=[]
 for lo,hi in sorted(ranges):
  if out and lo<=out[-1][1]+1:out[-1][1]=max(hi,out[-1][1])
  else:out.append([lo,hi])
 return out

C=P/'common';C.mkdir(exist_ok=True)
assert git('rev-parse',BASE+'^{tree}').decode().strip()==BTREE
assert git('rev-parse',V7+'^{tree}').decode().strip()==BTREE
linear=json.loads((B/'LINEAR-SOURCE-PLAN.json').read_text());changed=sorted(set(linear['first_commit_paths']+linear['second_commit_paths']))
assert len(changed)==13
for name,sha in [('01-cpuid-on-v7.patch','c1c7add220f014f24079104386908c5aa3d8d964f5224fc67b0fb82379fc423b'),('02-capture-after-cpuid.patch','83426cc72e517dee01139891a908c307719b0eb3ee57ca21fd25ac7954e663fc')]:assert rec(B/name)['sha256']==sha
manifest=json.loads((B/'SOURCE-MANIFEST.json').read_text());assert rec(B/'SOURCE-MANIFEST.json')['sha256']=='dfd21968df3b73ba28a7388f3504b8891e3f13035dab6f364e69fbe2947d1211'
source_files=[];aliases=[];gitlinks=[]
for row in manifest:
 p=SRC/row['path']
 if row['mode']=='160000':gitlinks.append(row);continue
 if row['mode']=='120000':
  assert p.is_symlink();target=os.readlink(p);assert hashlib.sha256(os.fsencode(target)).hexdigest()==row['sha256'];aliases.append(dict(path=str(p),target=target));continue
 a=rec(p);assert a['sha256']==row['sha256'] and bool(a['mode']&0o111)==(row['mode']=='100755');source_files.append(a)
put(C/'SOURCE-INPUTS.json',dict(files=source_files,symlinks=aliases,gitlinks=gitlinks,manifest=rec(B/'SOURCE-MANIFEST.json')))
full=[];patch=[];source_context=[];newdocs=[]
extra={
'reverie-kvm/src/runtime.rs':{'read_clock','has_cpuid_interception','execute','complete_injection','ordinary_injection_allowed','tail_injection_allowed','injection_can_be_nonreturning','injection_is_explicit_exit','run_with_tool','run_static_elf_with_tool_completion','run_static_elf_process_with_tool','poll_handler_or_signal'},
'reverie-kvm/src/vm.rs':{'new_with_memory_and_cpuid_policy','snapshot_process','from_process_snapshot','from_thread_state','exec_process','run_process_action_with_tool_inner','finish_tool_process_action_at_boundary','set_cpuid_interception','has_cpuid_interception','tool_instruction_exception','cpuid_instruction_exception','timestamp_instruction_exception','resume_tool_instruction','resume_instruction_registers','run_static_elf','run_static_elf_captured','prepare_captured_output','run_static_elf_process','finish_static_elf_thread','run_with_executor'},
'reverie-kvm/src/executor.rs':{'with_output','execute','try_from_elf','same_stdin_entry','update_from_elf','prepare_from_elf','install','fork_child','thread_child','thread_child_with_signal_observation','release_files_on_exit','replace_after_exec','take_exit','take_output','append_output','output_alias','output_alias_from_sets','set_output_alias','host_fd','guest_fd_path','open_guest_fd_path','insert_file_with_flags','duplicate_fd_at_or_above','duplicate_fd','close','close_range','fcntl'},
'reverie-kvm/tests/static_elf.rs':{'kvm_is_unavailable','kvm_available','static_elf','timestamp_assertion_program'},
}
for rel in changed:
 p=SRC/rel;after=p.read_text();lines=after.splitlines(keepends=True)
 before_result=subprocess.run(['/usr/bin/git','-C',str(L),'show',BASE+':'+rel],capture_output=True,timeout=30,env={**os.environ,'GIT_OPTIONAL_LOCKS':'0'})
 exists=before_result.returncode==0
 if not exists:assert b'does not exist' in before_result.stderr or b'exists on disk' in before_result.stderr,before_result.stderr
 before=before_result.stdout.decode() if exists else ''
 beforecopy=C/'before'/rel
 if exists:put(beforecopy,before)
 aftercopy=C/'after'/rel;put(aftercopy,after)
 full.append(dict(relative=rel,before=rec(beforecopy)if exists else None,after=rec(p),copy=rec(aftercopy),lines=len(lines),new=not exists))
 patch.extend(difflib.unified_diff(before.splitlines(True),lines,fromfile='a/'+rel if exists else '/dev/null',tofile='b/'+rel))
 if not exists:
  newdocs.append((rel.replace('/','-'),p));continue
 # Cover every nonempty changed after span with its complete function where applicable.
 fs=functions(lines);ranges=[];changed_spans=[]
 for tag,a,b,c,d in difflib.SequenceMatcher(a=before.splitlines(True),b=lines,autojunk=False).get_opcodes():
  if tag=='equal':continue
  changed_spans.append(dict(operation=tag,before=[a+1,b],after=[c+1,d]))
  for point in range(c,max(c+1,d)):
   containing=[f for f in fs if f['start']<=point<f['end']]
   if containing:
    fn=min(containing,key=lambda f:f['end']-f['start']);ranges.append((fn['start'],fn['end']))
   else:ranges.append((max(0,point-12),min(len(lines),point+13)))
 for fn in fs:
  if fn['name'] in extra.get(rel,set()):ranges.append((fn['start'],fn['end']))
 # Complete context enum and admission policy, not a partial enum selected by a hunk.
 if rel.endswith('/runtime.rs'):ranges.append((398,554))
 ranges=merge(ranges);text=''
 for lo,hi in ranges:text+=f'// SOURCE {p} lines {lo+1}..{hi}; full file SHA256 {rec(p)["sha256"]}\n'+''.join(lines[lo:hi])+'\n'
 ctx=C/'source-context'/rel.replace('/','--');put(ctx,text)
 source_context.append(dict(relative=rel,full_source=rec(p),context=rec(ctx),ranges=[dict(first_line=a+1,last_line=b)for a,b in ranges],changed_spans=changed_spans,complete_function_names=[f['name']for f in fs if any(a<=f['start']and b>=f['end']for a,b in ranges)]))
put(C/'COMPLETE-COMPOSITION.patch',''.join(patch))
put(C/'FULL-CHANGED-FILES.json',full);put(C/'SOURCE-CONTEXT-MAP.json',source_context)
# Complete public API context plus the unchanged instruction policy/fetch implementation.
context_docs=[]
for rel,names in [('reverie/src/guest.rs',{'has_cpuid_interception'}),('reverie/src/tool.rs',{'handle_cpuid_event'}),('reverie/src/subscription.rs',{'cpuid','has_cpuid'}),('reverie-kvm/src/timestamp.rs',None),('reverie-kvm/src/cpuid.rs',None),('reverie-kvm/src/parked_signal_runtime.rs',None),('reverie-kvm/tests/support/timestamp_terminal.rs',None)]:
 p=SRC/rel
 if not p.exists():continue
 if names is None:context_docs.append((rel.replace('/','-'),p));continue
 lines=p.read_text().splitlines(keepends=True);ranges=merge([(f['start'],f['end'])for f in functions(lines)if f['name']in names]);assert ranges,rel
 txt=''.join(f'// SOURCE {p} lines {a+1}..{b}; SHA256 {rec(p)["sha256"]}\n'+''.join(lines[a:b])+'\n'for a,b in ranges)
 cp=C/'source-context'/rel.replace('/','--');put(cp,txt);context_docs.append((rel.replace('/','-'),cp))
# Exact H39ac source only: unchanged consumer context, not a Hermit patch review or runtime claim.
h_records=[]
for rel in ['detcore/src/lib.rs','detcore/src/stat.rs','detcore/src/syscalls/files.rs']:
 p=C/'hermit-39ac'/rel;put(p,git('show',HSHA+':'+rel,repo=H).decode());h_records.append(rec(p))
 lines=p.read_text().splitlines(keepends=True)
 if rel.endswith('/stat.rs'):context_docs.append(('hermit-stat-complete',p));continue
 names={'handle_cpuid_event'}if rel.endswith('/lib.rs')else{'handle_fstat','handle_newfstatat','handle_statx','handle_stat','handle_lstat','handle_readlink','handle_readlinkat'}
 ranges=merge([(f['start'],f['end'])for f in functions(lines)if f['name']in names]);assert ranges,rel
 txt=''.join(f'// SOURCE {HSHA}:{rel} lines {a+1}..{b}; SHA256 {rec(p)["sha256"]}\n'+''.join(lines[a:b])+'\n'for a,b in ranges)
 cp=C/'source-context'/('hermit--'+rel.replace('/','--'));put(cp,txt);context_docs.append((rel.replace('/','-'),cp))
put(C/'HERMIT-CONTEXT.json',dict(commit=HSHA,files=h_records,scope='Existing consumer context only; no new Hermit review or runtime result'))
# Bound qualification preparation, caller, and preserved original refusal only. Active phase/result files are excluded.
Q=B/'qualification-v2';groups=json.loads((Q/'SELECTORS.json').read_text())['groups'];assert len(groups)==83
selected=[dict(phase=k,artifact=v['artifact'],name=v['names'][0],source=v['source'],purpose=v.get('purpose'))for k,v in groups.items()];assert len({(v['artifact'],v['name'])for v in selected})==83
put(C/'SELECTED-DECLARATIONS.json',dict(declarations=selected,counts=dict(collections.Counter(x['artifact']for x in selected)),planned_only=True,qualification_results_pending=True))
caller_names=['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','retain_artifacts.py','execute_continuation.py','observer/observer.py']
callers=[]
for name in caller_names:
 p=Q/name;assert p.exists(),p;callers.append((name.replace('/','-'),p))
for name in ['CALLER-DELTA.patch','CONTINUATION.json','FIRST-REFUSAL.json','SETUP.json','TARGET-ADMISSION.json','PREDECESSOR-ELFS.json']:
 callers.append((name,Q/name))
callers += [('original-caller-delta',B/'qualification-v1/CALLER-DELTA.patch'),('result-finalizer',B/'finalize_results_v2.py')]
put(C/'CALLER-INDEX.json',[dict(name=n,file=rec(p))for n,p in callers])
# Authenticate base source equality, complete source, patches and source selection without touching HEAD/index.
proof=dict(base=BASE,old_reviewed_head=V7,equal_tree=BTREE,source_manifest=rec(B/'SOURCE-MANIFEST.json'),files=len(source_files),symlinks=len(aliases),gitlinks=gitlinks,changed_paths=changed,whole_composition_diff=rec(C/'COMPLETE-COMPOSITION.patch'),original_patches=[rec(B/x)for x in ['01-cpuid-on-v7.patch','02-capture-after-cpuid.patch']],current_claim='Source-materialization identity only; committed successor and actual qualification final remain pending')
put(C/'SOURCE-AUTHENTICATION.json',proof)
put(C/'COVERAGE.md','''This preparation binds all thirteen changed files, complete before/after bytes, both original linear patches and a complete base-to-composition diff. New modules and all new test bodies are mandatory complete reads. Existing large files use complete changed functions plus explicit caller/lifecycle context, identified by original line ranges. Full original files remain bound and available for follow-up. This is not a claim to have read every unchanged line of the 41,095-line executor or the entire scheduler again.

Mandatory source context includes real CPUID MSR ownership/restore/readback, fault classification and instruction fetch, typed Tool dispatch/result application, terminal cleanup/admission and original timestamp neighbors; captured output setup before image/Tool consumption, root-owner lifetime, descriptor table synchronization, aliases/dup/proc reopening, fork/thread/exec/exit, stat/statx/readlink and error ordering. Complete raw-ABI guest fixtures and every changed assertion are in the patches/new modules. Qualification caller closure includes exact selectors, source/SCM/environment guards, offline/locked pinned compilation, artifact retention, leases, observer, result parser and original RefUnit refusal. Full final phase results are intentionally not frozen while execution is active.

Any necessary source beyond the prepared context must be read from the bound full source and reported honestly. Read completion is not a verdict. Existing H39ac consumer code is contextual only; no new Hermit behavior or full backend parity is inferred.
''')
# Session setup is isolated. There is deliberately no TARGET.json, inputs.json or executable launch authorization yet.
GOAL='''GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it
'''
lanes=[]
for lane in ['claude','native']:
 D=P/lane;D.mkdir(exist_ok=False);(D/'chunks').mkdir()
 previous=F/'claude'if lane=='claude'else O/'native'
 session='5d98403d-b8e9-42bf-8e2f-da7e7b70809c'if lane=='claude'else'01a0b38e-9e3c-7883-9816-65199c048935'
 helpers=['launch-review.py','audit-final.py']if lane=='claude'else['launch-guard.py','run_stage.py','audit_source.py','audit_split_reads.py']
 origins=[]
 for name in helpers:
  original=previous/name;shutil.copyfile(original,D/name);origins.append(dict(original=rec(original),deployed=rec(D/name),byte_identical=True))
 # Audit scope sentence is a reporting-only correction; predicates remain byte-identical.
 if lane=='claude':
  p=D/'audit-final.py';old=p.read_text();new=old.replace('Actual independent V7 timestamp source and finite evidence continuation. No whole-backend parity or clock-repair approval inferred.','Actual independent linear CPUID and capture composition source review. No whole-backend parity or clock-repair approval inferred.')
  assert old!=new;p.write_text(new);origins[-1]['deployed']=rec(p);origins[-1]['byte_identical']=False
  put(D/'AUDIT-WORDING-DELTA.patch',''.join(difflib.unified_diff(old.splitlines(True),new.splitlines(True),fromfile='prior/audit-final.py',tofile='prepared/audit-final.py')))
 for name in ['GROUNDING_AUTHENTICATION.json','GROUNDING_INPUTS.json']:
  shutil.copyfile(O/lane/name,D/name)
 grounding=json.loads((D/'GROUNDING_AUTHENTICATION.json').read_text());assert grounding['session_id']==session and grounding['current_hermit']==HSHA and grounding['scheduler_equals_own_original_complete_reading']
 own_ground=[check(r)for r in json.loads((D/'GROUNDING_INPUTS.json').read_text())]
 # Also authenticate all file records directly named by own grounding receipt.
 def refs(v):
  if isinstance(v,dict):
   if {'path','sha256'}<=set(v):yield v
   for x in v.values():yield from refs(x)
  elif isinstance(v,list):
   for x in v:yield from refs(x)
 for r in refs(grounding):check(r)
 report=previous/('CLAUDE-REPORT.md'if lane=='claude'else'01-source.final.txt');audit=previous/'FINAL-READBACK.json'
 custody=dict(session_id=session,own_report=rec(report),own_terminal_audit=rec(audit),own_grounding=rec(D/'GROUNDING_AUTHENTICATION.json'),prior_scope='Timestamp PR at 68569; no approval of new CPUID/capture code transferred',peer_reports_supplied=False,source_read_order='Prior source reading order limits remain in own original audit; canonical grounding separately authenticated')
 put(D/'OWN-PRIOR-CUSTODY.json',custody)
 docs=[('source-scope',C/'COVERAGE.md'),('source-identity',C/'SOURCE-AUTHENTICATION.json'),('own-grounding',D/'GROUNDING_AUTHENTICATION.json'),('own-prior-report',report),('full-composition-diff',C/'COMPLETE-COMPOSITION.patch')]
 docs+= [('new-'+n,p)for n,p in newdocs]
 docs+= [('context-'+r['relative'].replace('/','-'),Path(r['context']['path']))for r in source_context]
 docs+=context_docs
 docs+=[('selected-declarations',C/'SELECTED-DECLARATIONS.json')]
 docs+=[('caller-'+n,p)for n,p in callers]
 index=[]
 for name,p in docs:
  lines=p.read_text().splitlines(keepends=True);parts=[];start=0
  while start<len(lines):
   end=start;size=0
   while end<len(lines)and end-start<160 and(size+len(lines[end].encode())<=9000 or end==start):size+=len(lines[end].encode());end+=1
   chunk=D/'chunks'/f'{name}-{start+1:05d}-{end:05d}.txt';put(chunk,''.join(lines[start:end]));parts.append(dict(**rec(chunk),first_line=start+1,last_line=end));start=end
  index.append(dict(name=name,source=rec(p),lines=len(lines),chunks=parts))
 put(D/'COMPLETE_READS-PREPARED.json',index if lane=='claude'else dict(documents=[dict(name=d['name'],document=d['source'],lines=d['lines'],chunks=d['chunks'])for d in index],mandatory_documents=len(index),mandatory_chunks=sum(len(d['chunks'])for d in index),mandatory_lines=sum(d['lines']for d in index)))
 put(D/'READ-INDEX-PREPARED.txt','Complete the listed bounded chunks in order; own canonical grounding is already separately authenticated. Final committed-head/result documents will be appended before launch, and a new final index will name those inputs. Full source is indexed in common/FULL-CHANGED-FILES.json and common/SOURCE-INPUTS.json.\n\n'+'\n'.join(f'{i+1}. {d["name"]}: {d["source"]["path"]} SHA256 {d["source"]["sha256"]}\n'+''.join('   '+c['path']+'\n'for c in d['chunks'])for i,d in enumerate(index)))
 target=dict(status='PREPARATION ONLY; NOT A LAUNCH TARGET',session_id=session,stage='01-source'if lane=='native'else'linear-parity-composition',expected_actual_model='gpt-5.6-sol'if lane=='native'else'claude-opus-5[1m]',grounding=str(S/('ignored/kvm-timer-native-cli-review-v1-20260918'if lane=='native'else'ignored/kvm-timer-review-grounding-v2-20260918')),base=BASE,base_tree=BTREE,head='@@HEAD@@',tree='@@TREE@@',frozen_source=str(SRC),source_manifest=rec(B/'SOURCE-MANIFEST.json'),scope='Independent whole linear CPUID plus captured-pipe identity source review; no activation, clock-policy repair or whole-backend parity claim',qualification='@@FINAL_EVIDENCE@@',hermit_context=HSHA,mandatory_documents=len(index),mandatory_chunks=sum(len(d['chunks'])for d in index),mandatory_lines=sum(d['lines']for d in index))
 put(D/'TARGET.template.json',target)
 permission='Use only Read/Grep/Glob; no Bash, plan-file creation, writes or delegation.'if lane=='claude'else'Use read-only bounded file reads; no source edits, builds, tests, guests, network, delegation, or external writes.'
 prompt=f'''Continue your own session {session}. Independently review the complete Reverie successor {BASE}..@@HEAD@@, tree @@TREE@@. The base is the landed timestamp source from https://github.com/rrnewton/reverie/pull/586, byte-identical tree {BTREE} to your previously reviewed 68569ed7. The new source target is {SRC}, manifest SHA256 dfd21968df3b73ba28a7388f3504b8891e3f13035dab6f364e69fbe2947d1211. This is the complete new CPUID plus capture composition, not only its final commit or tests. The final head/source proof and actual qualification are in @@FINAL_EVIDENCE@@. No other reviewer's findings, verdict, or classifications are supplied.

Use your OWN authenticated prior complete paper → complete H39ac scheduler → vision → roadmap grounding. Inspect OWN-PRIOR-CUSTODY and the grounding authentication; do not borrow the author's reading or another reviewer. Your old timestamp report remains separate and supplies no new-source approval. Start with @@INDEX@@ and read every bounded mandatory chunk completely. Follow the bound full files for any missing caller. Full changed files are bound even when the index provides complete affected functions from a much larger unchanged file. State exactly what you read and any limits; do not claim a listed order unless your actual calls follow it. The supplied current H39ac consumer source is context, not a new Hermit source verdict or tested integration.

Review the entire production and test change on correctness, Linux/x86 semantics, deterministic Tool behavior, liveness and ownership:
1. CPUID MSR capability detection, exact get/set counts, per-real-vCPU ownership, stale/readback/partial failure, restoration and opt-out. Check instruction decoding/prefix/LOCK/length/fetch fault priority, CPL/long-mode envelope, input truncation and full register/RFLAGS/RIP result semantics, TF, coexistence with RDTSC/RDTSCP, fresh child versus exec/reused vCPU behavior, default/Host/Tool loops and advertised has_cpuid_interception. Do not equate native CPUID masking with callback support.
2. The shared Instruction callback state must preserve the landed timestamp behavior. Trace original callback ownership, asynchronous RPC/injection, ordinary versus tail admission, exact terminal status and hook cleanup, failure paths and child lifecycle. The V7 worker terminal fixture remains intact; its admission test changes only private context naming. Ordinary returning write remains allowed while tail write is refused. A pure MSR/decoder control is not an end-to-end Tool control; inspect the real fixture evidence distinctly.
3. Captured output now reserves actual pipe identities. Trace setup before image/Tool consumption, both real pipe failures and low-standard-fd relocation failure, private CLOEXEC endpoint ownership, root lifetime/drop/unwind, stream identity versus guest fd alias/OFD identity, shared threads/private fork, exec/close/replacement/dup/proc reopen, authoritative table synchronization and stat/statx/readlink/proc symlink routes. Check unchanged byte capture and error/mask/ordering behavior. Real host inode/device identity is not by itself deterministic virtualization; evaluate the unchanged Tool consumer separately. Do not generalize this change to pipe readiness or physical I/O.
4. Inspect every changed assertion, fixture, skip and metadata oracle against the base. An old synthetic identity assertion changes because the intended semantics now use a live pipe identity; decide whether exact native identity, alias and negative controls satisfy that requirement rather than merely weakening it. Real EMFILE/closed-standard-fd/error cleanup controls must remain real, not replaced by a mocked refusal. Full raw six-argument admission and unrelated descriptor-width/signal behavior are unchanged.
5. Audit actual qualification identity and limitations. The final selection is 83 unique declarations (43 lib, 34 static, two vmcall, four read-clock), across 92 planned phases. Read actual final outcomes; no pending result is a pass. Four fresh retained emitted harnesses and linked library/source/loader provenance must match this composition. Preserve the initial raw 137 RefUnit accounting refusal as unqualified; the fresh private-lease continuation is separate. Inspect raw skip messages and REVERIE_REQUIRE_KVM=1. Inventories, declarations and physical fixture modes are different populations. Do not add overlapping predecessor test totals or infer full backend parity, a Hermit timer fix, clock-origin repair or new cancellation intervals.

Return findings first with precise source references and concrete blocking defects, or say none. Explicitly assess all four goalpost targets. Give separate conclusions for deterministic behavior, Linux/x86/descriptor semantics and evidence limits, then an unambiguous APPROVE or CHANGES REQUESTED for the WHOLE base-to-head CPUID+capture composition. Identify review-protocol triggers from the actual diff; no syscall was newly added merely because an existing callback gains backend support. Prior classifications are not binding on this new diff. Reading completion or predecessor approval is not approval of this source.

Read-only. {permission} No model override or external publication. Do not inspect another review lane's report or mutable qualification output. Final report must be assistant text; no product mutation or reviewer-written files. The fixed launch bound is 1800 seconds and 64 MiB output under the established controls; do not substitute an unfinished review for a verdict.

{GOAL}'''
 put(D/'prompt.template.txt',prompt)
 put(D/'HELPER-ORIGINS.json',origins)
 # Minimal own custody closure, full current source and prepared inputs. Old whole review packets are not recursively imported.
 records={r['path']:r for r in source_files};links=list(aliases)
 def add(p):r=rec(p);records[r['path']]=r
 for r in own_ground:records[r['path']]=r
 for r in refs(grounding):records[r['path']]=check(r)
 for f in [report,audit,*[Path(r['path'])for r in h_records]]:add(f)
 for d in index:
  add(d['source']['path'])
  for c in d['chunks']:add(c['path'])
 for f in D.iterdir():
  if f.is_file():add(f)
 for f in C.rglob('*'):
  if f.is_file():add(f)
 for f in [B/'README.md',B/'LINEAR-SOURCE-PLAN.json',B/'SOURCE-TARGET.json',B/'SOURCE-MANIFEST.json',B/'01-cpuid-on-v7.patch',B/'02-capture-after-cpuid.patch',B/'RUN-ORDER.json',Q/'SELECTORS.json',Q/'CALLER-INPUTS.json',Q/'RUNNER_ORIGINS.json',Path(__file__)]:add(f)
 for r in records.values():check(r)
 put(D/'inputs-PREPARED.json',dict(files=sorted(records.values(),key=lambda x:x['path']),symlinks=links))
 lanes.append(dict(lane=lane,session=session,target=rec(D/'TARGET.template.json'),prompt=rec(D/'prompt.template.txt'),inputs=rec(D/'inputs-PREPARED.json'),read_index=rec(D/'READ-INDEX-PREPARED.txt'),complete_reads=rec(D/'COMPLETE_READS-PREPARED.json'),helpers=rec(D/'HELPER-ORIGINS.json'),files=len(records),aliases=len(links),mandatory_documents=len(index),mandatory_chunks=target['mandatory_chunks'],mandatory_lines=target['mandatory_lines']))
plan=dict(status='PREPARED ONLY; final committed head/source proof and immutable qualification records required before root authorizes launch',base=BASE,source_manifest=rec(B/'SOURCE-MANIFEST.json'),lanes=lanes,qualification_expected=dict(phases=92,declarations=83,artifacts=['lib','static','vmcall','read-clock'],result_directory=str(B/'qualification-result-v2'),no_current_result_claim=True),finalization='Use a new final-v1 directory; preserve all prepared templates. Append complete final source/head proof, RESULTS, raw execution audit and required raw phase/error outputs to both read indices; bind all final INPUTS. No launch until root checks final prompts/inputs/commands.',commands={lane:['with-proxy','env','PYTHONOPTIMIZE=0','/usr/bin/python3','-B',str(P/'final-v1'/lane/('launch-review.py'if lane=='claude'else'launch-guard.py'))]for lane in ['claude','native']},bounds=dict(wall_seconds=1800,claude_per_output_file_bytes=67108864,native_combined_raw_bytes=67108864,all_uid_cli_cap=15,model_override=False),admission='Launch one lane only after root authorization, verify actual session/model/effective policy; fresh all-UID census in unchanged caller before the second actual launch. Do not precredit a preparation-time census.',terminal='Own both actual handles through termination; preserve raw failures, actual reports, source-before/after, all tool errors/read-order/truncation/coverage and session/policy audits. No peer report supplied or inferred approval.')
put(P/'PLAN-PREPARED.json',plan)
put(P/'PREPARATION-READBACK.json',dict(plan=rec(P/'PLAN-PREPARED.json'),source=rec(C/'SOURCE-AUTHENTICATION.json'),complete_source_files=len(source_files),aliases=len(aliases),changed_paths=len(changed),lanes=lanes,executed_reviewers=0,qualification_or_product_mutation=False))
print(json.dumps(dict(readback=rec(P/'PREPARATION-READBACK.json'),plan=rec(P/'PLAN-PREPARED.json'),lanes=[{k:v for k,v in r.items()if k in ['lane','files','aliases','mandatory_documents','mandatory_chunks','mandatory_lines']}for r in lanes]),indent=2))
