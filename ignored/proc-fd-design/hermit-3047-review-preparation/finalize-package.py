from pathlib import Path
import ast, datetime, difflib, hashlib, json, os, shutil, stat, subprocess
p=Path(__file__).resolve().parent
manifest=json.loads((p/'snapshot-manifest.json').read_text())
ledger=Path('/home/newton/work/dev-hermit/ignored/kvm-parity-20260914-codex/next-work/queue-drain-20260916/parent-ledger/pin-mount-aa7ea482-review')
def sha(data): return hashlib.sha256(data).hexdigest()
def row(path):
 data=path.read_bytes()
 return {'path':str(path),'bytes':len(data),'sha256':sha(data),'mode':oct(stat.S_IMODE(path.stat().st_mode))}
for name in ['REVIEW.md','VENDOR-PATH-CLARIFICATION.md','content-readback.json','generator-and-commit-readback.json']:
 q=p/'evidence/native-hermit-review'/name;q.parent.mkdir(parents=True,exist_ok=True)
 assert not q.exists()
 data=(ledger/name).read_bytes();q.write_bytes(data);q.chmod(stat.S_IMODE((ledger/name).stat().st_mode))
 if name=='REVIEW.md':assert sha(data)=='f08938074e4707ff9570045935092743fb7cac883b21c2121c927fc2c405c5da'
 manifest['files'].append({**row(q),'relative_path':str(q.relative_to(p)),
                           'origin':{'path':str(ledger/name),'sha256':sha(data)}})
(p/'snapshot-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
candidate={'repository':'https://github.com/rrnewton/hermit','pull_request':'https://github.com/rrnewton/hermit/pull/3047',
 'root':str(p/'source'),'base':manifest['hermit']['base'],'head':manifest['hermit']['head'],
 'tree':manifest['hermit']['tree'],'candidate_patch_sha256':manifest['hermit']['patch_sha256'],
 'changed_paths':manifest['hermit']['changed_paths'],'linked_reverie':manifest['reverie'],
 'files':[{'path':str(Path(x['path']).relative_to(p/'source')),'bytes':x['bytes'],'sha256':x['sha256'],'mode':x['mode'],
           'origin':x['origin']} for x in manifest['files'] if x['relative_path'].startswith('source/')]}
(p/'candidate-binding.json').write_text(json.dumps(candidate,indent=2)+'\n')
donor=(p/'architecture/donor-launch-review.py').read_text()
current=(p/'launch-review.py').read_text()
(p/'launcher.diff').write_text(''.join(difflib.unified_diff(donor.splitlines(True),current.splitlines(True),
                                    fromfile='approved-donor/launch-review.py',tofile='prepared-aa7/launch-review.py')))
# Parsing is an offline preparation syntax check, never executing the launcher.
ast.parse(current)
tools=[]
for name in ['/usr/bin/python3','timeout','with-proxy','claude']:
 resolved=Path(name if name.startswith('/') else shutil.which(name)).resolve()
 tools.append({'entrypoint':name,'resolved_path':str(resolved),**row(resolved)})
plan={'state':'prepared_only_not_released_or_executed','default_launcher_refuses_without_execution_argument':True,
 'proposed_released_argv':['/usr/bin/python3','-B',str(p/'launch-review.py'),'--execute-reviewed-aa7ea482'],
 'argument_is_not_authorization':'Root must review the exact prompt/caller/input binding and release the command before use.',
 'review_command':['timeout','--signal=TERM','--kill-after=10s','900s','with-proxy','claude','-p',
                   '--no-session-persistence','--output-format','stream-json','--verbose','--permission-mode','dontAsk',
                   '--permission-prompts','none','--tools','Read,Grep,Glob','--allowedTools','Read,Grep,Glob','--strict-mcp-config'],
 'cwd':str(p/'source/hermit'),'wall_seconds':900,'kill_after_seconds':10,'per_output_file_bytes':67108864,
 'outputs':[str(p/n) for n in ['launch.json','stdout.jsonl','stderr.log','exit.json']],
 'no_test_or_guest_execution':True,'no_source_or_ref_or_pr_operations':True,
 'hermit_base':candidate['base'],'hermit_head':candidate['head'],'hermit_tree':candidate['tree'],
 'candidate_binding_sha256':sha((p/'candidate-binding.json').read_bytes()),
 'snapshot_manifest_sha256':sha((p/'snapshot-manifest.json').read_bytes()),
 'tools_current_metadata':tools,'read_tools_only':True,
 'actual_evidence_cutoff':'Four aa7 mount config methods and DAG --check completed; later tests/runtime pending. The complete Reverie library is 426 pass/1 fail.',
 'normal_disjoint_future_main':'b03fd6c16a438060f0013d58948643d8f3d81ab5 is preserved context, not this source target.'}
(p/'execution-plan.json').write_text(json.dumps(plan,indent=2)+'\n')
# All supplied input bytes/modes, including the caller itself, are checked at both ends.
inputs=[{k:x[k] for k in ['path','bytes','sha256','mode']} for x in manifest['files']]
for name in ['snapshot-manifest.json','candidate-binding.json','prompt.txt','REVIEW-GUIDE.md','EVIDENCE.md',
             'launch-review.py','launcher.diff','execution-plan.json']:
 inputs.append(row(p/name))
assert len({x['path'] for x in inputs})==len(inputs)
(p/'input-binding.json').write_text(json.dumps(inputs,indent=2)+'\n')
block='''GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it'''
assert block in (p/'prompt.txt').read_text()
for x in inputs:
 q=Path(x['path']);actual=row(q)
 assert all(actual[k]==x[k] for k in ['path','bytes','sha256','mode']),str(q)
# Re-read every source/base copy from the exact Git object again, independently of source paths.
verified_git=0
for x in manifest['files']:
 origin=x.get('origin',{})
 if 'git_blob' in origin:
  actual=subprocess.check_output(['git','cat-file','blob',origin['git_blob']],cwd=origin['repository'],
                env={**os.environ,'GIT_NO_LAZY_FETCH':'1','GIT_OPTIONAL_LOCKS':'0'})
  assert actual==Path(x['path']).read_bytes(),x['path']
  entry=subprocess.check_output(['git','ls-tree',origin['revision'],'--',origin['path']],cwd=origin['repository'],
                env={**os.environ,'GIT_NO_LAZY_FETCH':'1','GIT_OPTIONAL_LOCKS':'0'}).decode().strip()
  assert entry.split('\t')[0].split()[0]==origin['git_mode']
  verified_git+=1
for q in plan['outputs']: assert not Path(q).exists(),q
own=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917')
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=own).decode().strip()=='696f0476aa46cf29e31b947a89379d80b4542ce3'
assert not subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=own)
binding={'prepared_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),
 'head':candidate['head'],'tree':candidate['tree'],'base':candidate['base'],
 'input_count':len(inputs),'input_bytes':sum(x['bytes'] for x in inputs),'git_object_copies_independently_rechecked':verified_git,
 'source_copy_count':len(candidate['files']),'complete_patch_sha256':candidate['candidate_patch_sha256'],
 'all_bound_files_modes_and_bytes_match':True,'all_launch_outputs_absent':True,'own_source_index_clean_at_696':True,
 'no_external_reviewer_or_native_or_guest_was_launched':True,
 'files':{n:row(p/n) for n in ['prompt.txt','launch-review.py','launcher.diff','execution-plan.json',
 'candidate-binding.json','input-binding.json','snapshot-manifest.json','REVIEW-GUIDE.md','EVIDENCE.md']}}
(p/'PREPARATION-READBACK.json').write_text(json.dumps(binding,indent=2)+'\n')
print(json.dumps(binding,indent=2))
