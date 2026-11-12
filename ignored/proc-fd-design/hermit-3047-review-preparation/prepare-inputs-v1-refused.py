from pathlib import Path
import datetime, hashlib, json, os, stat, subprocess

out = Path(__file__).resolve().parent
hermit = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916')
reverie = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917')
parent = Path('/home/newton/work/dev-hermit')
ledger = parent/'ignored/kvm-parity-20260914-codex/next-work/queue-drain-20260916/parent-ledger'
root_slot = parent/'worktrees/slots/kvm-parent-reader-support-20260916'
base = '35af18f34a3238117eecac8b439ac20e13316715'
head = 'aa7ea4827b8328345e715d76518d7f2205e41110'
pin = '596b9adee8473dc0a7e62dce18580eead3d0c5c9'
source = out/'source'
assert not source.exists(), 'immutable source already prepared'
records = []

def digest(b): return hashlib.sha256(b).hexdigest()
def git(repo, *args):
    return subprocess.check_output(['git',*args], cwd=repo, env={**os.environ,'GIT_NO_LAZY_FETCH':'1','GIT_OPTIONAL_LOCKS':'0'})
def put(rel, data, mode=0o644, origin=None):
    q=out/rel; q.parent.mkdir(parents=True,exist_ok=True)
    with q.open('xb') as f: f.write(data)
    q.chmod(mode)
    row={'path':str(q),'relative_path':rel,'bytes':len(data),'sha256':digest(data),'mode':oct(mode)}
    if origin is not None: row['origin']=origin
    records.append(row)
    return row

def copy_file(src, rel, expected=None):
    data=src.read_bytes()
    if expected: assert digest(data)==expected, (str(src),digest(data),expected)
    return put(rel,data,stat.S_IMODE(src.stat().st_mode),{'path':str(src),'sha256':digest(data)})

def copy_git(repo, rev, name, prefix):
    entry=git(repo,'ls-tree',rev,'--',name).decode().strip()
    if not entry: raise RuntimeError('missing Git input '+rev+':'+name)
    left, actual_name=entry.split('\t'); mode,typ,blob=left.split()
    assert actual_name==name and typ=='blob' and mode in ['100644','100755'], entry
    return put(prefix+'/'+name,git(repo,'cat-file','blob',blob),int(mode[-3:],8),
        {'repository':str(repo),'revision':rev,'path':name,'git_blob':blob,'git_mode':mode})

changed=git(hermit,'diff','--name-only',base,head).decode().splitlines()
assert len(changed)==21
assert git(hermit,'rev-parse',head+'^{tree}').decode().strip()=='40d69bea9282cbcbf161a2736bcb39108a1d53db'
assert git(hermit,'rev-parse',head+'^').decode().strip()=='df8f668f9956797ffaeb72a26a938619c0be5df9'
patch=git(hermit,'diff','--binary','--full-index',base,head)
assert digest(patch)=='96502f9880df67b8423b1ed111c65f1f08feb0eede6c30657db25f281f92083a'
put('complete-hermit.patch',patch,origin={'repository':str(hermit),'base':base,'head':head})
put('complete-hermit.stat',git(hermit,'diff','--stat',base,head))
put('hermit-commits.txt',git(hermit,'log','--reverse','--format=fuller','--no-decorate',base+'..'+head))
put('hermit-tree.txt',git(hermit,'ls-tree','-r',head))
for name in changed:
    copy_git(hermit,head,name,'source/hermit')
    if git(hermit,'ls-tree',base,'--',name).strip(): copy_git(hermit,base,name,'base/hermit')
hermit_context=[
 'AGENTS.md','Cargo.toml','rust-toolchain',
 'detcore/src/config.rs','detcore/src/fd.rs','detcore/src/lib.rs','detcore/src/procfs.rs',
 'detcore/src/record_or_replay.rs','detcore/src/resources.rs','detcore/src/stat.rs','detcore/src/tool_global.rs',
 'detcore-model/src/config.rs','detcore-model/src/fd.rs','detcore-model/src/procfs.rs',
 'hermit-cli/src/bin/hermit/container.rs','hermit-cli/src/bin/hermit/run.rs','hermit-cli/src/fd.rs',
 'scripts/check-reverie-pin.rs','ci/manifest-plan/src/validation_dag.rs',
 'ci/publish-hermit-e2e-artifact.sh']
# The toolchain filename is taken from the immutable tree, not guessed.
hermit_context.remove('rust-toolchain')
for name in ['rust-toolchain','rust-toolchain.toml']:
    if git(hermit,'ls-tree',head,'--',name).strip(): hermit_context.append(name)
for name in hermit_context:
    if name not in changed: copy_git(hermit,head,name,'source/hermit')
reverie_paths=[
 'Cargo.toml','reverie-kvm/Cargo.toml',
 'reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/fdinfo.rs',
 'reverie-kvm/src/lib.rs','reverie-kvm/src/memory.rs','reverie-kvm/src/proc_mounts.rs',
 'reverie-kvm/src/runtime.rs','reverie-kvm/src/vm.rs',
 'reverie/src/guest.rs','reverie/src/lib.rs','reverie/src/tool.rs']
assert git(reverie,'rev-parse',pin+'^{tree}').decode().strip()=='bb2c88a1f0b37e264535693f2b72d77b2502dc5f'
for name in reverie_paths: copy_git(reverie,pin,name,'source/reverie')
put('landed-reverie.commit',git(reverie,'show','--no-patch','--format=fuller',pin))
put('landed-reverie-component.patch',git(reverie,'diff','--binary','--full-index','4866241e15c18bdbd717865c504b9facdcaf1ae0',pin))
for name in ['AGENTS.md','.skills/code-review/SKILL.md','.skills/deterministic-scheduling-review/SKILL.md']:
    copy_file(parent/name,'policy/'+name)

artifact_dir=hermit/'ignored/recovery/mount-integration-596b9ade'
for name in ['FINAL-SOURCE-AND-CALLERS.json','DAG-COMPARISON.json','MOUNT-APPLICATION-READBACK.json',
             'PLAN.md','mount-18-generate-dag.json','mount-18-generate-dag.log','mount-18-generate-dag-stderr.log',
             'mount-18-scope-readback.json','mount-integration-commit-ack.json','mount-integration-commit-ack.log',
             'mount-integration-commit.json','mount-integration-commit.log','COMMIT-ACK-PREPARATION.json']:
    copy_file(artifact_dir/name,'evidence/hermit-integration/'+name)
for name in ['REPORT.md','stage02-readback.json','main-delta.patch','main-commits','main-readback.json',
             'FULL-LIBRARY-RESULT.md','full-library-readback.json','full-library-failure.txt']:
    copy_file(ledger/'pin-mount-596b-stage02-audit'/name,'evidence/pin-and-main-audit/'+name)
mount=hermit/'ignored/recovery/kvm-mount-provenance-proposal/v2'
for name in ['PLAN.md','complete-binding.json','hermit-mount-provenance-with-counts.patch','targeted-checks.json']:
    copy_file(mount/name,'evidence/mount-v2/'+name)
for name in ['CLOSED-STDIN-FDINFO-ROUTE-7628.md','closed-stdin-fdinfo-route.json','CLOSED-STDIN-FDINFO-MOUNT-PROVENANCE-7628.md']:
    copy_file(hermit/'ignored/recovery'/name,'evidence/historical-hermit/'+name)
old=hermit/'ignored/recovery/main-increment-independent-review/closed-standard-input-20260917'
for name in ['REPORT.md','CANDIDATE-NARROW-v5-HANDOFF.md','NARROW-BEFORE-MEASUREMENT-v4-RESULT.md',
             'NARROW-BEFORE-MEASUREMENT-v5-RESULT-CORRECTED.md']:
    copy_file(old/name,'evidence/historical-hermit/'+name)
copy_file(old/'measurement-observer/measurement-candidate-7628/candidate-kvm/stderr',
          'evidence/historical-hermit/candidate-7628-fdinfo.stderr','a4a97928b16c079869b9241e9cb88bce00795d60f7f241dcce7889f2ec978fb3')

own=reverie/'ignored/proc-fd-design'
for rel in ['source-v3/binding.json','source-v3/selected-tests.json',
            'cargo-v3/execution-readback.json','cargo-v3/clippy/execution-readback.json',
            'native-full-v1/RESULTS.md','native-full-v1/execution-readback.json',
            'landing-preparation/merge-1/independent-readback/CONTENT-READBACK.json']:
    copy_file(own/rel,'evidence/reverie/'+rel)
# Retain actual output, receipts, listing and named test outcomes, without copying the 106-MB ELF.
for rel,prefix in [('cargo-v3/execution-readback.json','native37'),
                   ('cargo-v3/clippy/execution-readback.json','clippy'),
                   ('native-full-v1/execution-readback.json','native427')]:
    readback=json.loads((own/rel).read_text())
    for row in readback['files']:
        q=Path(row['path'])
        if 'measurement-observer/' in str(q) or q.name in ['native-outcomes.json','native-summary-readback.json']:
            suffix=str(q).split('measurement-fdinfo-native-20260917/',1)[-1] if 'measurement-fdinfo-native-20260917/' in str(q) else q.name
            copy_file(q,'evidence/reverie/raw/'+prefix+'/'+suffix,row['sha256'])
reviews=ledger/'proc-fd-design-review-24cd'
for rel in ['REVIEW.md','external-review-readback/REPORT.md','final-source-v3-review/REVIEW.md',
            'external-continuation-audit/REPORT.md']:
    copy_file(reviews/rel,'evidence/reverie/reviews/'+rel)
copy_file(root_slot/'ignored/reverie-proc-fd-review-24cd-v3-continuation/REVIEW.md',
          'evidence/reverie/reviews/external-continuation-APPROVE.md',
          '303e20dafd2f82a3af1c1cba0e2160f4666864eef6fd9cd23f582640a0471786')
copy_file(root_slot/'ignored/reverie-proc-fd-review-24cd-v3-complete/REVIEW.md',
          'evidence/reverie/reviews/external-original-CHANGES-REQUESTED.md')
copy_file(root_slot/'ignored/reverie-proc-fd-review-24cd-v3-continuation/launch-review.py',
          'architecture/donor-launch-review.py')
manifest={'prepared_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'files':records,
          'hermit':{'base':base,'head':head,'tree':'40d69bea9282cbcbf161a2736bcb39108a1d53db','changed_paths':changed,
                    'context_paths':hermit_context,'patch_sha256':digest(patch)},
          'reverie':{'revision':pin,'tree':'bb2c88a1f0b37e264535693f2b72d77b2502dc5f','paths':reverie_paths},
          'not_executed':'This script only copies immutable Git objects and retained evidence into the assigned ignored directory.'}
(out/'snapshot-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(json.dumps({'files':len(records),'bytes':sum(x['bytes'] for x in records),'manifest_sha256':digest((out/'snapshot-manifest.json').read_bytes()),'patch_sha256':digest(patch)},indent=2))
