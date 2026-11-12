from pathlib import Path
import difflib,hashlib,json,re,stat
root=Path(__file__).resolve().parent
shell=json.loads((root/'shell-changes.json').read_text())
test=json.loads((root/'consumer-test-changes.json').read_text())
rows=[]
for c in shell['changes']:
 rows.append((c['path'],Path(c['source']),Path(c['destination']),c['source_git_mode'],c))
rows.append(('hermit-cli/tests/verification_report_consumers.rs',Path(test['before']['path']),Path(test['after']['path']),'100644',None))
rows.sort()
manifest=[]; patches=[]
def sha256(b):return hashlib.sha256(b).hexdigest()
def blob(b):return hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()
def apply_unified(before,lines):
 source=before.decode().splitlines(keepends=True);result=[];cursor=0;i=2
 while i<len(lines):
  header=lines[i];m=re.fullmatch(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\n',header);assert m,header
  old_start=int(m[1]);old_count=int(m[2] or 1);new_count=int(m[4] or 1)
  start=old_start-1 if old_count else old_start
  assert start>=cursor
  result.extend(source[cursor:start]);cursor=start;i+=1;consumed=produced=0
  while i<len(lines) and not lines[i].startswith('@@ '):
   line=lines[i];kind=line[0];body=line[1:]
   if kind in ' -':
    assert source[cursor]==body,(cursor,line,source[cursor]);cursor+=1;consumed+=1
   if kind in ' +':result.append(body);produced+=1
   assert kind in ' +-';i+=1
  assert consumed==old_count and produced==new_count
 result.extend(source[cursor:]);return ''.join(result).encode()
for path,src,dst,gitmode,c in rows:
 before=src.read_bytes();after=dst.read_bytes()
 assert before.endswith(b'\n') and after.endswith(b'\n')
 before_mode=stat.S_IMODE(src.stat().st_mode);after_mode=stat.S_IMODE(dst.stat().st_mode)
 assert before_mode==after_mode==int(gitmode[-3:],8),(path,oct(before_mode),oct(after_mode),gitmode)
 if c:
  assert sha256(before)==c['before_sha256'] and sha256(after)==c['after_sha256']
  restored=after.decode()
  for delta in reversed(c['exact_text_deltas']):
   assert restored.count(delta['after'])==delta['occurrences']
   restored=restored.replace(delta['after'],delta['before'])
  assert restored.encode()==before
  assert after.count(b'"$VERIFICATION_REPORT_BIN" canonical-match ')==1
  assert b'"$VERIFICATION_REPORT_BIN" matched ' not in after
 else:
  assert sha256(before)==test['before']['sha256'] and sha256(after)==test['after']['sha256']
 basecopy=root/'base'/path;basecopy.parent.mkdir(parents=True,exist_ok=True)
 with basecopy.open('xb') as f:f.write(before)
 basecopy.chmod(before_mode)
 lines=list(difflib.unified_diff(before.decode().splitlines(keepends=True),after.decode().splitlines(keepends=True),fromfile='a/'+path,tofile='b/'+path,n=3))
 assert lines and apply_unified(before,lines)==after
 patch='diff --git a/'+path+' b/'+path+'\nindex '+blob(before)+'..'+blob(after)+' '+gitmode+'\n'+''.join(lines)
 patches.append(patch)
 manifest.append({'path':path,'base_kind':'immutable M2 preview-v10' if c is None else 'immutable Hermit 14d63ed54b7284b7f8bc29d44c7610a809815621','source':str(src),'base_copy':str(basecopy),'preview':str(dst),'git_mode':gitmode,'before':{'bytes':len(before),'sha256':sha256(before),'git_blob':blob(before)},'after':{'bytes':len(after),'sha256':sha256(after),'git_blob':blob(after)},'mode_unchanged':True,'unified_hunks_apply_in_memory_exactly':True})
patch=''.join(patches)
with (root/'candidate.patch').open('x') as f:f.write(patch)
with (root/'PREVIEW-MANIFEST.json').open('x') as f:json.dump({'scope':'seven shell readers, KVM wording, seven of thirteen consumer entries, one new source/actual-reader control','files':manifest,'live_application':False,'product_execution':False,'formatters_run':False},f,indent=2);f.write('\n')
print(json.dumps({'candidate_patch_bytes':len(patch.encode()),'candidate_patch_sha256':sha256(patch.encode()),'preview_manifest_sha256':sha256((root/'PREVIEW-MANIFEST.json').read_bytes()),'changed_paths':len(manifest),'retained_thirteen_consumer_paths':test['retained_thirteen_paths'],'added_declared_test':test['added_declared_test_name']},indent=2))
