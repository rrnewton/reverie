"""Artifact-only preparation from immutable Git objects; never launches a reviewer."""
from pathlib import Path
import copy, datetime, difflib, hashlib, json, os, shutil, subprocess, sys
P=Path(__file__).resolve().parent
OLD=P.parent/'hermit-3047-review-final-followup'
REPO=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916')
BASE_OLD='158a89f6217b25db9540237f9c1e256cdbaf785c'
HEAD_OLD='a88f948021a24c6a1ea808f868516c0691c8819a'
BASE='231228c010541561a81dcb1353ac200f5c791b1a'
EXPECTED_TREE='b06a3d19d4e15e31542aeef742d5f81679df2a99'
if len(sys.argv)!=2 or len(sys.argv[1])!=40:
    raise SystemExit('prepare.py requires the actual immutable rebased head; no reviewer is launched')
HEAD=sys.argv[1]
ENV=dict(os.environ,GIT_NO_LAZY_FETCH='1',GIT_OPTIONAL_LOCKS='0')
def git(*args):
    r=subprocess.run(['git',*args],cwd=REPO,env=ENV,check=True,capture_output=True)
    return r.stdout
def sha(data):return hashlib.sha256(data).hexdigest()
def write(name,data):
    q=P/name;q.parent.mkdir(parents=True,exist_ok=True)
    q.write_bytes(data if isinstance(data,bytes) else data.encode());return q

def tree(ref):
    result={}
    for raw in git('ls-tree','-r','-z',ref).split(b'\0'):
        if not raw:continue
        meta,path=raw.split(b'\t',1);mode,kind,oid=meta.decode().split()
        result[path.decode()]={'mode':mode,'kind':kind,'object':oid}
    return result

def blob_oid(data):return hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()

def commits(base,head):
    result=[]
    for ref in git('rev-list','--reverse',base+'..'+head).decode().splitlines():
        raw=git('cat-file','commit',ref);headers,message=raw.split(b'\n\n',1)
        author=next(s for s in headers.splitlines() if s.startswith(b'author ')).decode()
        parents=[s[7:].decode() for s in headers.splitlines() if s.startswith(b'parent ')]
        result.append({'commit':ref,'parents':parents,'author':author,'message':message.decode(),'raw_commit_sha256':sha(raw)})
    return result

def changed_lines(patch):
    files={};path=None
    for line in patch.decode().splitlines(keepends=True):
        if line.startswith('diff --git '):
            path=line.rstrip('\n').split(' b/',1)[1];files[path]=[]
        elif path is not None and (line.startswith(('+','-')) and not line.startswith(('+++','---')) or line.startswith(('old mode ','new mode ','new file mode ','deleted file mode '))):
            files[path].append(line)
    return files

actual_tree=git('rev-parse',HEAD+'^{tree}').decode().strip()
assert actual_tree==EXPECTED_TREE,(HEAD,actual_tree)
assert git('merge-base',BASE,HEAD).decode().strip()==BASE
base_old,old,main,actual=map(tree,[BASE_OLD,HEAD_OLD,BASE,HEAD])
incoming_paths=sorted(p for p in set(base_old)|set(main) if base_old.get(p)!=main.get(p))
assert incoming_paths==['ci/dag/validate.json','ci/manifest-plan/src/validation_dag_static.rs','hermit-cli/tests/liteinst_advanced.rs']
expected=copy.deepcopy(old)
expected_files={}
for path in incoming_paths:
    if old.get(path)==base_old.get(path):
        expected[path]=main[path]
        expected_files[path]=git('show',BASE+':'+path)
    else:
        before=git('show',HEAD_OLD+':'+path)
        if path=='ci/dag/validate.json':
            source=b'"NEXTEST_EXPECTED_EXECUTED": "505"';target=b'"NEXTEST_EXPECTED_EXECUTED": "507"'
            assert before.count(source)==2
            after=before.replace(source,target)
        elif path=='ci/manifest-plan/src/validation_dag_static.rs':
            after=before
            for suffix in ['', '_on_host']:
                source=('("test.regular_crates'+suffix+'", 505)').encode()
                assert after.count(source)==1
                after=after.replace(source,source.replace(b'505',b'507'))
        else:raise AssertionError(path)
        expected[path]={**old[path],'object':blob_oid(after)};expected_files[path]=after
assert actual==expected,[(p,actual.get(p),expected.get(p)) for p in set(actual)|set(expected) if actual.get(p)!=expected.get(p)]
for path,data in expected_files.items():
    assert git('show',HEAD+':'+path)==data
    write('expected-union/'+path,data)
old_patch=git('diff','--no-ext-diff','--binary',BASE_OLD,HEAD_OLD)
new_patch=git('diff','--no-ext-diff','--binary',BASE,HEAD)
old_changes,new_changes=changed_lines(old_patch),changed_lines(new_patch)
assert set(old_changes)==set(new_changes)
assert old_changes==new_changes,'authored added/deleted bytes changed'
old_commits,new_commits=commits(BASE_OLD,HEAD_OLD),commits(BASE,HEAD)
assert len(old_commits)==len(new_commits)==3
assert [(x['author'],x['message']) for x in old_commits]==[(x['author'],x['message']) for x in new_commits]
assert new_commits[0]['parents']==[BASE]
assert all(x['parents']==[new_commits[i-1]['commit']] for i,x in enumerate(new_commits) if i)
write('complete-hermit.patch',new_patch)
write('prior-authored.patch',old_patch)
write('incoming-main.patch',git('diff','--no-ext-diff','--binary',BASE_OLD,BASE))
write('follow-up.diff',git('diff','--no-ext-diff','--binary',HEAD_OLD,HEAD))
write('incoming-commits.txt',git('log','--reverse','--format=fuller',BASE_OLD+'..'+BASE))
write('tracing-history.patch',git('show','--format=fuller','1d9c9094dea0e7a4a85bb641165088bded1386bd','--','hermit-cli/src/bin/hermit/tracing.rs'))
write('authored-commits.json',json.dumps({'prior':old_commits,'actual':new_commits},indent=2)+'\n')
write('whole-tree.json',json.dumps({'base':main,'prior':old,'actual':actual,'expected_union':expected},indent=2)+'\n')
for name,digest in [('REVIEW.md','ca36cfc5135f6d664195e456067936967e5a748449b4eee4e449b3c9d709ed07'),('COMPLETION-READBACK.json','675aaf2d583ca4758127d61b2fc72883297d85511690ec6b2dd2025ed2eaa638')]:
    data=(OLD/name).read_bytes();assert sha(data)==digest
    write('prior-a88-'+name,data)
write('prior-a88-RESULTS.md',(OLD/'RESULTS.md').read_bytes())
source_old=json.loads((OLD/'candidate-binding.json').read_text())
rows=[]
for row in source_old['files']:
    q=Path(source_old['root'])/row['path'];data=q.read_bytes()
    assert sha(data)==row['sha256'] and len(data)==row['bytes'] and oct(q.stat().st_mode&0o777)==row['mode']
    if row['path'].startswith('hermit/'):
        rel=row['path'][7:];data=git('show',HEAD+':'+rel)
        mode=actual[rel]['mode'];origin={'repository':str(REPO),'revision':HEAD,'path':rel,'git_blob':actual[rel]['object'],'git_mode':mode}
    else:origin=row['origin'];mode=origin['git_mode']
    dest=write('source/'+row['path'],data);os.chmod(dest,int(mode,8)&0o777)
    rows.append({'path':row['path'],'bytes':len(data),'sha256':sha(data),'mode':oct(dest.stat().st_mode&0o777),'origin':origin})
for rel in ['hermit-cli/tests/liteinst_advanced.rs','hermit-cli/tests/common/liteinst.rs','hermit-cli/tests/common/hermit_binary.rs','hermit-cli/src/bin/hermit/tracing.rs']:
    if any(x['path']=='hermit/'+rel for x in rows):continue
    data=git('show',HEAD+':'+rel);mode=actual[rel]['mode'];dest=write('source/hermit/'+rel,data);os.chmod(dest,int(mode,8)&0o777)
    rows.append({'path':'hermit/'+rel,'bytes':len(data),'sha256':sha(data),'mode':oct(dest.stat().st_mode&0o777),'origin':{'repository':str(REPO),'revision':HEAD,'path':rel,'git_blob':actual[rel]['object'],'git_mode':mode}})
source={'repository':'rrnewton/hermit','pull_request':'https://github.com/rrnewton/hermit/pull/3047','root':str(P/'source'),'base':BASE,'head':HEAD,'tree':actual_tree,'candidate_patch_sha256':sha(new_patch),'files':rows}
write('candidate-binding.json',json.dumps(source,indent=2)+'\n')
changed_copies=[x['path'] for x in rows if any(y['path']==x['path'] and (y['sha256'],y['mode'])!=(x['sha256'],x['mode']) for y in source_old['files'])]
readback={'base':BASE,'head':HEAD,'tree':actual_tree,'prior_approved':HEAD_OLD,'prior_base':BASE_OLD,'whole_tree_entries':len(actual),'expected_tree':EXPECTED_TREE,'actual_equals_full_expected_union':actual==expected,'incoming_paths':incoming_paths,'authored_paths':sorted(new_changes),'authored_pathset_equal':True,'authored_added_deleted_bytes_equal':True,'raw_authored_patch_equal':old_patch==new_patch,'prior_patch_sha256':sha(old_patch),'actual_patch_sha256':sha(new_patch),'three_ordered_author_lines_and_full_messages_equal':True,'all_other_tree_entries_equal_prior':all(actual.get(p)==old.get(p) for p in set(actual)|set(old) if p not in incoming_paths),'changed_existing_context_copies':changed_copies,'source_copy_count':len(rows),'no_execution_inferred':True}
write('composition-readback.json',json.dumps(readback,indent=2)+'\n')
prompt=(P/'prompt.template.txt').read_text().replace('@HEAD@',HEAD).replace('@TREE@',actual_tree).replace('@PATCH_SHA@',sha(new_patch)).replace('@PATCH_BYTES@',str(len(new_patch)))
write('prompt.txt',prompt)
launcher=(OLD/'launch-review.py').read_text().replace('--execute-reviewed-a88f9480','--execute-reviewed-'+HEAD[:8])
write('launch-review.py',launcher)
write('launcher.diff',''.join(difflib.unified_diff((OLD/'launch-review.py').read_text().splitlines(True),launcher.splitlines(True),fromfile='prior/launch-review.py',tofile='composition/launch-review.py')))
plan=json.loads((OLD/'execution-plan.json').read_text())
plan.update(state='prepared_only_not_released_or_executed',proposed_released_argv=['/usr/bin/python3','-B',str(P/'launch-review.py'),'--execute-reviewed-'+HEAD[:8]],cwd=str(P/'source/hermit'),hermit_base=BASE,hermit_head=HEAD,hermit_tree=actual_tree,candidate_binding_sha256=sha((P/'candidate-binding.json').read_bytes()),outputs=[str(P/x) for x in ['launch.json','stdout.jsonl','stderr.log','exit.json']],source_scope='Exact three-commit source-preserving rebase onto main231: two regular count sites and LiteInst PID assertion/comment; full authored patch and whole union bound.',prior_literal_source_approval={'head':HEAD_OLD,'report':str(P/'prior-a88-REVIEW.md'),'sha256':'ca36cfc5135f6d664195e456067936967e5a748449b4eee4e449b3c9d709ed07'},actual_evidence_cutoff='aa7 measured native19/fmt/Clippy634/build and2descriptor+3complete-mode passes. Original24-method current run pending; actual1839 zero-node tool-root refusal retained. Reverie426/1 unchanged.')
for row in plan['tools_current_metadata']:
    resolved=Path(shutil.which(row['entrypoint']) if not row['entrypoint'].startswith('/') else row['entrypoint']).resolve(strict=True)
    data=resolved.read_bytes();row.update(resolved_path=str(resolved),path=str(resolved),bytes=len(data),sha256=sha(data),mode=oct(resolved.stat().st_mode&0o777))
write('execution-plan.json',json.dumps(plan,indent=2)+'\n')
write('plan.diff',''.join(difflib.unified_diff((OLD/'execution-plan.json').read_text().splitlines(True),(P/'execution-plan.json').read_text().splitlines(True),fromfile='prior/execution-plan.json',tofile='composition/execution-plan.json')))
write('prompt.diff',''.join(difflib.unified_diff((OLD/'prompt.txt').read_text().splitlines(True),prompt.splitlines(True),fromfile='prior/prompt.txt',tofile='composition/prompt.txt')))
prior_inputs=json.loads((OLD/'input-binding.json').read_text());bound={}
for name,digest in [('validate-kvm-closed-stdin-a88f9480-frozen.json','fee1158194a7cb4d0b7c6163322d22126ab2fb92271ade05f1b7a2c9ba51b1dc'),('validate-kvm-closed-stdin-a88f9480-frozen.service-result.json','bf780df553c03a0245e65da75f600d7b7949864b24ff7b620fe0b6bf47e10fce')]:
    q=P.parent/'frozen-cleanup-contract-review/evidence'/name;data=q.read_bytes();assert sha(data)==digest
    bound[str(q)]={'path':str(q),'bytes':len(data),'sha256':sha(data),'mode':oct(q.stat().st_mode&0o777)}
for row in prior_inputs:
    q=Path(row['path']);data=q.read_bytes()
    assert (len(data),sha(data),oct(q.stat().st_mode&0o777))==(row['bytes'],row['sha256'],row['mode']),str(q)
    bound[str(q)]=row
for q in sorted(P.rglob('*')):
    if q.is_file() and q.name not in {'input-binding.json','PREPARATION-READBACK.json','STATUS.md'}:
        data=q.read_bytes();bound[str(q)]={'path':str(q),'bytes':len(data),'sha256':sha(data),'mode':oct(q.stat().st_mode&0o777)}
for row in plan['tools_current_metadata']:
    bound[row['path']]={k:row[k] for k in ['path','bytes','sha256','mode']}
write('input-binding.json',json.dumps(list(bound.values()),indent=2)+'\n')
result={**readback,'prepared_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'input_count':len(bound),'prompt_bytes':len(prompt.encode()),'input_binding_sha256':sha((P/'input-binding.json').read_bytes()),'prompt_sha256':sha(prompt.encode()),'launcher_sha256':sha(launcher.encode()),'plan_sha256':sha((P/'execution-plan.json').read_bytes()),'launcher_diff_sha256':sha((P/'launcher.diff').read_bytes()),'no_reviewer_launched':True}
write('PREPARATION-READBACK.json',json.dumps(result,indent=2)+'\n')
write('STATUS.md','Prepared and input-bound; no reviewer launch. Root must read and release the exact command.\n')
print(json.dumps(result,indent=2))
