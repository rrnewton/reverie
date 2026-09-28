from pathlib import Path
import subprocess,hashlib,json,difflib,os,re,shutil
D=Path(__file__).resolve().parent
H='/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917'
REV='14d63ed54b7284b7f8bc29d44c7610a809815621'
def sha(data): return hashlib.sha256(data).hexdigest()
def desc(p):
 data=p.read_bytes();return dict(bytes=len(data),sha256=sha(data),mode=oct(p.stat().st_mode & 0o777))
for f in ['hermit-cli/src/lib.rs','hermit-cli/src/error.rs','hermit-cli/src/kvm_failure_tests.rs']:
 src=D/'cli-part/changed'/f;dst=D/'changed'/f;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(src,dst)
records=[];patch=[]
for changed in sorted((D/'changed').rglob('*')):
 if not changed.is_file():continue
 f=str(changed.relative_to(D/'changed'))
 tree=subprocess.check_output(['git','-C',H,'ls-tree',REV,'--',f]).decode().strip()
 if tree:
  mode,kind,blob=tree.split('\t')[0].split();assert kind=='blob';old=subprocess.check_output(['git','-C',H,'cat-file','blob',blob])
  bp=D/'base'/f;bp.parent.mkdir(parents=True,exist_ok=True)
  if bp.exists():assert bp.read_bytes()==old,f
  else:bp.write_bytes(old)
  bp.chmod(int(mode,8)&0o777);changed.chmod(int(mode,8)&0o777)
  before=desc(bp);before.update(git_blob=blob,git_mode=mode)
 else:
  old=b'';before=None;mode='100644';changed.chmod(0o644)
 new=changed.read_bytes();assert old!=new,f
 diff=list(difflib.unified_diff(old.decode().splitlines(keepends=True),new.decode().splitlines(keepends=True),fromfile='a/'+f if before else '/dev/null',tofile='b/'+f))
 header=['diff --git a/'+f+' b/'+f+'\n']
 if not before:header+=['new file mode 100644\n']
 patch+=header+diff
 # Independently consume the exact unified hunks in memory, without git apply.
 result=[];cursor=0;i=2
 while i<len(diff):
  m=re.match(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@',diff[i]);assert m,(f,diff[i]);at=int(m.group(1));at=at-1 if at else 0
  oldlines=old.decode().splitlines(keepends=True);result+=oldlines[cursor:at];cursor=at;i+=1
  while i<len(diff) and not diff[i].startswith('@@ '):
   line=diff[i];i+=1
   if line.startswith((' ','-')):assert oldlines[cursor]==line[1:],(f,cursor);cursor+=1
   if line.startswith((' ','+')):result.append(line[1:])
 result+=old.decode().splitlines(keepends=True)[cursor:]
 assert ''.join(result).encode()==new,f
 oldnames=re.findall(r'#\[(?:tokio::)?test[^\n]*\][\s\S]*?\b(?:async\s+)?fn\s+(\w+)',old.decode())
 newnames=re.findall(r'#\[(?:tokio::)?test[^\n]*\][\s\S]*?\b(?:async\s+)?fn\s+(\w+)',new.decode())
 assert not set(oldnames)-set(newnames),f
 records.append(dict(path=f,before=before,after=desc(changed),new_source_test_names=[n for n in newnames if n not in oldnames],removed_source_test_names=[]))
(D/'candidate.patch').write_text(''.join(patch))
(D/'FILES.json').write_text(json.dumps(dict(base_commit=REV,files=records),indent=2)+'\n')
inputs=[]
for p in sorted((D/'base').rglob('*')):
 if not p.is_file():continue
 f=str(p.relative_to(D/'base'));tree=subprocess.check_output(['git','-C',H,'ls-tree',REV,'--',f]).decode().strip();mode,kind,blob=tree.split('\t')[0].split()
 original=subprocess.check_output(['git','-C',H,'cat-file','blob',blob]);assert p.read_bytes()==original,f;p.chmod(int(mode,8)&0o777)
 inputs.append(dict(path=f,git_blob=blob,git_mode=mode,copy=desc(p)))
for name,source in [
 ('root-REPORT.md','/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored/kvm-sigchld-current-source-root-20260917/REPORT.md'),
 ('retained-PLAN.md','/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored/kvm-sigchld-follow-up-preparation-20260917/PLAN.md'),
 ('fixture-mapping-REPORT.md','/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/sigchld-current-source-preparation-20260917/fixture-mapping/REPORT.md')]:
 destination=D/'grounding'/name;shutil.copy2(source,destination);inputs.append(dict(path=name,source=source,copy=desc(destination)))
(D/'INPUTS.json').write_text(json.dumps(dict(base_commit=REV,base_tree=subprocess.check_output(['git','-C',H,'rev-parse',REV+'^{tree}']).decode().strip(),records=inputs),indent=2)+'\n')
(D/'READBACK.json').write_text(json.dumps(dict(patch=desc(D/'candidate.patch'),checks=['Every base file byte-equal to its immutable Git blob','Every exact unified hunk applied in memory equals retained changed file','Original source test function names retained','No product, guest, test, formatter, or build executed'],execution_status='source-only'),indent=2)+'\n')
print(json.dumps(dict(patch=desc(D/'candidate.patch'),changed_files=len(records),new_source_test_names=[n for r in records for n in r['new_source_test_names']]),indent=2))
