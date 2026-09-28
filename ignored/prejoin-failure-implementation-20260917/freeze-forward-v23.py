from pathlib import Path
import hashlib,json,os,subprocess
slot=Path.cwd();area=slot/'ignored/prejoin-failure-implementation-20260917';out=area/'source-v23-preparation';out.mkdir();old=area/'source-v22-preparation';base='526c21cf06ef9e5098ec9002b93e40e2022e798f';head='9db60ab95587d4cb5e0438dfeca409471eb9baf5';tip='9dfffd7ef114c10ede7e092107a36a6da2fb3df0'
def git(*argv):return subprocess.check_output(['git',*argv])
def h(data):return hashlib.sha256(data).hexdigest()
def write(path,data):
 path.parent.mkdir(parents=True,exist_ok=True)
 with path.open('xb') as f:f.write(data)
def put(path,obj):write(path,(json.dumps(obj,indent=2)+'\n').encode())
def record(path):
 path=Path(path);s=path.stat();return {'path':str(path),'resolved_path':str(path.resolve()),'bytes':s.st_size,'mode':s.st_mode&0o7777,'sha256':h(path.read_bytes())}
def tree(rev):
 rows={}
 for raw in git('ls-tree','-rz','--full-tree',rev).split(b'\0'):
  if raw:
   meta,path=raw.split(b'\t',1);mode,kind,obj=meta.decode().split();rows[path.decode()]={'mode':mode,'git_blob':obj}
 return rows
assert git('rev-parse','HEAD').decode().strip()==head
assert not git('diff','--name-only').strip() and not git('diff','--cached','--name-only').strip()
headtree=tree(head);basetree=tree(base);manifest=[]
for path,row in headtree.items():
 row={'path':path,**row}
 if row['mode']=='160000':row['gitlink']=row['git_blob']
 else:
  source=slot/path;data=os.fsencode(os.readlink(source)) if row['mode']=='120000' else source.read_bytes()
  assert hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()==row['git_blob'],path
  row['sha256']=h(data)
 manifest.append(row)
put(out/'tracked-source-manifest.json',manifest)
patch=git('diff','--no-ext-diff','--binary',base,head);assert patch==(old/'candidate.patch').read_bytes()
write(out/'candidate.patch',patch);write(out/'increment.patch',git('diff','--no-ext-diff','--binary',tip,head))
selection=(old/'selected-tests.json').read_bytes();write(out/'selected-tests.json',selection)
changed=git('diff','--name-only',base,head).decode().splitlines();oldbinding=json.loads((old/'binding.json').read_text());assert changed==[r['path'] for r in oldbinding['files']]
sourcefiles=[];copied=[]
support=[p for p in headtree if p=='reverie-kvm/tests/static_elf.rs' or p.startswith('reverie-kvm/tests/support/')]
additional=['Cargo.toml','rust-toolchain.toml','reverie/src/backend_stats.rs','reverie-kvm/tests/README.md']
for path in changed:
 data=git('show',head+':'+path);source=slot/path
 sourcefiles.append({'path':path,'mode':format(source.stat().st_mode&0o7777,'o'),'bytes':len(data),'sha256':h(data)})
 assert sourcefiles[-1]==next(r for r in oldbinding['files'] if r['path']==path)
for path in sorted(set(changed+support+additional)):
 for kind,rev,entries in [('committed-head',head,headtree),('committed-base',base,basetree)]:
  if path not in entries:
   copied.append({'revision':rev,'path':path,'absent':True});continue
  data=git('show',rev+':'+path);destination=out/kind/path;write(destination,data)
  copied.append({'revision':rev,'path':path,'git_mode':entries[path]['mode'],'git_blob':entries[path]['git_blob'],'copy':str(destination),'bytes':len(data),'sha256':h(data)})
put(out/'committed-copies.json',copied)
for path in support:assert headtree[path]==basetree[path],path
report=f'''# Reverie pre-join failure repair on current main

Final committed review target is {base}..{head}, tree {git('rev-parse','HEAD^{tree}').decode().strip()}, on branch codex/kvm-proc-fd-identity-20260917. The normal forward rebase retains two linear commits. The prior d99853df plus appended 9dfffd7e history remains reachable through refs/rescue/kvm-prejoin-v22-before-forward-20260917 and a verified bundle. No public push occurred.

All 11 candidate files and the complete candidate patch are byte-identical to accepted source v22. The full new source manifest covers {len(manifest)} entries. All 31 upstream changes, including SaBRe and LiteInst work, are preserved. The root manifest, toolchain and local Cargo.lock remain unchanged, while the actual Reverie backend_stats dependency source changed upstream. Therefore final compilation and actual emitted ELF comparison remain required. Old binaries are not relabelled as new-head builds. Committed base/head Git-blob copies for the 11 candidate paths, original {len(support)} integration support files and selected dependency inputs are listed in committed-copies.json; absent new base paths are explicit.

A fatal worker publishes the paired typed cause and real process/thread identity before retirement and joins. Ordinary Guest RPC interruption and process-scoped callback interruption are separate: a healthy independent process can complete real writes and naturally exit when a default GlobalTool has not terminated it, while pending ordinary RPCs cannot hold up owned joins. Explicit GlobalTool terminal notification retains priority. Recorded handler signals are consumed before callback readiness; interrupted RPC futures drop their inner requests and remain permanently pending across repeated polls. Consuming GlobalRPC hooks remain usable after failure. Pending child gates carry explicit ordinary/fatal causes; constructed state has one consuming owner. Typed completion promotes only the authoritative Arc with valid worker identity and retains distinct cleanup causes, original diagnostics and status semantics.

Prior source v22 passed all 44 selected native controls from 453 actual library identities, workspace format, and default-feature reverie-kvm all-targets Clippy with -D warnings. Its unchanged original 4 VM and 22 static methods now have accepted execution: 25 first attempts and one retry after the retained observer accounting refusal. This includes the original ten exec error modes, all leader-exit methods, actual fork-child writes/status/waitability and distinct error retention. The accepted service total was 9.701618 CPU / 31.953927814 summed observed wall seconds; the refused service is additional. All 27 qualification service instances were independently inactive/empty. The observer, assertions, order, hardware admission and bounds were unchanged. These measurements retain v22 source/ELF attribution. Forward-head workspace/all-feature checks and executable comparison are pending, separately recorded when terminal.

The actual Claude request for changes on d99853df remains preserved. Subsequent exact source reviews accepted the confirmed diagnostic, explicit gate-cause, worker-identity, caught-panic, typed normalization, healthy-peer status, independent-process/RPC, select-Ready and repeated-poll corrections. This packet does not substitute their prior verdicts for adversarial review of this exact committed head.

Remaining limits: this is the Reverie half; final Hermit scheduler/initialized-VM/pthread integration is separate. The prior whole-suite fatal_worker_ro_delayed_waiter failure remains outside selected success. Successful exec cancelling an already-started sibling parked in ordinary RPC is a separate existing gap; no fatal error is fabricated for successful exec. Arbitrary panic unwinding may bypass consuming hooks; the repair covers the existing caught worker panic path. Scratch-hide/callback dual-error cleanup and virtual-PID SIGCHLD/fork-tree remain separate. Per-exit async allocation/mutex cost is unmeasured. No whole-suite, Hermit strict INFO, repeat determinism or canonical cross-backend parity claim follows.

Human review trigger 2 applies to GlobalTool failure hooks, completion ownership and core error propagation. The coordinated Hermit scheduler transition separately meets trigger 4. Keep post-facto-human-review disclosure and actual independent Claude/Codex review bindings; no full-DAG receipt is a hard prerequisite for landing.
'''
write(out/'REPORT.md',report.encode())
binding={'schema':1,'base':base,'base_tree':git('rev-parse',base+'^{tree}').decode().strip(),'head':head,'tree':git('rev-parse','HEAD^{tree}').decode().strip(),'branch':'codex/kvm-proc-fd-identity-20260917','files':sourcefiles,'patch_sha256':h(patch),'patch_bytes':len(patch),'increment_sha256':h((out/'increment.patch').read_bytes()),'increment_scope':'Only the 31 preserved upstream paths from pre-rebase qualified tip to final head; no KVM candidate change.','report_sha256':h((out/'REPORT.md').read_bytes()),'source_manifest_sha256':h((out/'tracked-source-manifest.json').read_bytes()),'source_file_count':len(manifest),'selected_tests_sha256':h(selection),'selected_no_vm_count':44,'original_qualification_vm_count':4,'original_qualification_static_count':22,'committed_copies_sha256':h((out/'committed-copies.json').read_bytes()),'qualified_previous_binding_sha256':h((old/'binding.json').read_bytes()),'previous_committed_tip':tip,'source_preparation':'Committed forward source; candidate bytes identical to v22. Final-head build/lint/executable comparison not yet executed.'}
put(out/'binding.json',binding)
evidence=[]
for version,names in [('cargo-v16',['REPORT.md','RESULT.json','run-1/retained-elf-v22/binding.json']),('lint-v13',['REPORT.md','RESULT.json','run-1/summary.json']),('qualification-build-v5',['REPORT.md','RESULT.json','run-1/retained-elf-v22/binding.json']),('qualification-execution-v4',['REPORT.md','RESULT.json','run-1/summary.json']),('qualification-execution-v5',['REPORT.md','RESULT.json','UNION-RESULT.json','run-1/summary.json','run-1/independent-terminal-union.json']),('append-v22-preparation',['COMMIT-READBACK.json','RESCUE-READBACK.json','FORWARD-ASSESSMENT.json','FORWARD-READBACK.json'])]:
 for name in names:evidence.append(record(area/version/name))
evidence.append(record(Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored/prejoin-claude-review-d99853df/REPORT.md')))
put(out/'prior-evidence.json',evidence)
prompt=f'''Review the exact committed Reverie target {base}..{head}. Read the complete candidate.patch, committed base/head files and relevant callers/tests, full source binding, forward/commit records and prior evidence; independently seek regressions in the actual lifecycle and RPC paths. Sources are frozen; use read-only inspection only. Do not run builds/tests/guests or publish anything. Root owns separate bounded final checks and publication.

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it

Compare all changed controls against their actual base and preserve the unchanged original static_elf population. The v22 accounting refusal is retained: one repeated method was accepted later, so never describe 26 clean first attempts. Source-identical prior results do not invent pending forward execution.

Prior actual Claude d998 review requested changes. Review each confirmed correction rather than assuming prior reviewer approval: diagnostic compatibility; ordinary/fatal gate cause independent of hook-return timing; all actual host worker, cached status/error and caught-panic producers before joins; paired event/cause identity; typed primary normalization without discarding distinct shared cleanup causes; healthy live-thread cancellation status; independent fork completion versus pending ordinary Guest RPC; explicit HandlerSignal priority with Ready alternatives; and stable repeated RPC polling without fabricated responses. Check publication remains synchronous before local/process wake, terminal GlobalTool priority, normal Start/status/error behavior, one consuming owner across setup/spawn/refusal, actual worker-before-leader semantics and weak GlobalState ownership. Inspect the real normal and negative controls and their limits.

Do not invent a fatal failure to fix the separate successful-exec pending-sibling cancellation gap. Preserve the explicit arbitrary-panic, whole-suite, scratch-hide, SIGCHLD and performance limits from REPORT.md. New GlobalTool hooks are core abstraction trigger 2; coordinated Hermit terminal scheduler work is separate trigger 4. No API-only completion or 100 percent determinism/parity claim is justified.

Return concrete findings with file/line, impact and required correction; explicitly assess assertion weakening, widened tolerance/exemption/skips/comparator changes, failure relabelling, and deleted checks. State approve or changes requested for this exact head only, with evidence scope and residual limits. No human-facing message delivery or external publication is requested.
'''
write(out/'REVIEW-PROMPT.md',prompt.encode())
print(json.dumps({name:record(out/name) for name in ['binding.json','candidate.patch','REPORT.md','tracked-source-manifest.json','committed-copies.json','prior-evidence.json','REVIEW-PROMPT.md']},indent=2))
