from pathlib import Path
import hashlib, json, os, subprocess
root=Path.cwd(); evidence=root/'ignored/parity-composition-v1'; base='6e3915b70a71657a08f0028b3f70d6074116206a'
def git(*args): return subprocess.check_output(['git',*args],cwd=root)
def write(name,obj):
 p=evidence/name
 with p.open('x') as f: json.dump(obj,f,indent=2); f.write('\n')
def sha(b): return hashlib.sha256(b).hexdigest()
assert git('rev-parse','HEAD').decode().strip()==base
assert git('branch','--show-current').decode().strip()=='codex/kvm-parity-land-20260918'
assert not git('status','--porcelain','--untracked-files=no')
manifest_bytes=(evidence/'SOURCE-MANIFEST.json').read_bytes()
assert sha(manifest_bytes)=='dfd21968df3b73ba28a7388f3504b8891e3f13035dab6f364e69fbe2947d1211'
manifest=json.loads(manifest_bytes)
for item in manifest:
 p=evidence/'source'/item['path']
 if item['mode']=='160000': assert p.is_dir() and not any(p.iterdir()); continue
 b=os.fsencode(os.readlink(p)) if item['mode']=='120000' else p.read_bytes()
 assert sha(b)==item['sha256'],item['path']
plan=json.loads((evidence/'LINEAR-SOURCE-PLAN.json').read_bytes())
messages=[
'''Dispatch KVM CPUID instructions through Tool callbacks

[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]

Enable CPUID faulting for each vCPU and route guest CPUID instructions to
Tool::handle_cpuid. Preserve the previous vCPU MSR values and restore them
on initialization failure and teardown. Keep host-owned execution separate
from guest instruction dispatch and preserve cancellation at callback exits.

Add instruction, callback, fork/exec, terminal, and worker-exit coverage.
The composed CPUID and captured-pipe changes passed 83 exact KVM tests,
formatting, core/ptrace checks, and Clippy. This is component evidence;
full Hermit parity and clock-accounting failures remain open.
''',
'''Preserve native pipe identity for captured KVM output

[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]

Keep a native pipe open for each captured output stream and expose its
actual device and inode through guest descriptor metadata. Share ownership
across descriptor copies while keeping the private descriptors above the
standard descriptor range and close-on-exec.

Cover descriptor copies, closed standard descriptors, concurrent setup,
unwind, and descriptor exhaustion. The composed CPUID and captured-pipe
changes passed 83 exact KVM tests and the existing static checks. Preserve
the first metadata setup timeout as a separate infrastructure refusal;
these results do not establish full Hermit backend parity.
''']
commits=[]
for i,key in enumerate(['first_commit_paths','second_commit_paths']):
 patch=evidence/Path(plan['patches'][i]['path']).name
 assert sha(patch.read_bytes())==plan['patches'][i]['sha256']
 git('apply','--check','--index',str(patch)); git('apply','--index',str(patch))
 staged=git('diff','--cached','--name-only','-z').decode().rstrip('\0').split('\0')
 assert sorted(staged)==sorted(plan[key]),staged
 git('diff','--cached','--check')
 msg=evidence/f'commit-{i+1}.txt'
 with msg.open('x') as f: f.write(messages[i])
 result=subprocess.run(['git','commit','-F',str(msg)],cwd=root,text=True,capture_output=True)
 write(f'commit-{i+1}-result.json',{'returncode':result.returncode,'stdout':result.stdout,'stderr':result.stderr})
 assert result.returncode==0,result.stderr
 head=git('rev-parse','HEAD').decode().strip(); commits.append(head)
 print(f'commit {i+1}: {head}',flush=True)
 assert not git('status','--porcelain','--untracked-files=no')
entries={}
for row in git('ls-tree','-rz','--full-tree','HEAD').split(b'\0'):
 if not row: continue
 fields,name=row.split(b'\t',1); mode,kind,oid=fields.decode().split()
 entries[name.decode()]={'mode':mode,'kind':kind,'oid':oid}
assert set(entries)=={x['path'] for x in manifest}
blobs=[(x,entries[x['path']]) for x in manifest if x['mode']!='160000']
batch=subprocess.run(['git','cat-file','--batch'],cwd=root,input=('\n'.join(e['oid'] for x,e in blobs)+'\n').encode(),capture_output=True,check=True).stdout
pos=0
for item,entry in blobs:
 end=batch.index(b'\n',pos); header=batch[pos:end].decode().split(); size=int(header[2]); pos=end+1
 body=batch[pos:pos+size]; pos+=size+1
 assert entry['mode']==item['mode'] and header[0]==entry['oid'] and sha(body)==item['sha256'],item['path']
assert pos==len(batch)
links=[]
for item in manifest:
 if item['mode']=='160000':
  entry=entries[item['path']]
  old=git('ls-tree',base,'--',item['path']).decode().strip().split()
  assert entry['mode']=='160000' and entry['oid']==old[2],item['path']
  links.append({'path':item['path'],'oid':entry['oid']})
proof={'base':base,'commits':commits,'head':commits[-1],'tree':git('rev-parse','HEAD^{tree}').decode().strip(),'manifest_sha256':sha(manifest_bytes),'manifest_entries':len(manifest),'all_git_blobs_equal_qualified_source':True,'unchanged_gitlinks':links,'tracked_checkout_clean':True,'qualification_results_sha256':sha((evidence/'qualification-result-v2/RESULTS.json').read_bytes()),'remote_untouched':True}
write('COMMITTED-SOURCE-PROOF.json',proof)
print(json.dumps(proof,indent=2))
