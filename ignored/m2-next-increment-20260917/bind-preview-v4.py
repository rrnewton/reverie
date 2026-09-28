from pathlib import Path
import hashlib,json,re,difflib,subprocess,stat
D=Path(__file__).resolve().parent
base=json.loads((D/'BASE.json').read_text());repo=base['repository'];sha=base['commit']
def h(b):return hashlib.sha256(b).hexdigest()
def apply_patch(name):
 lines=(D/name).read_text().splitlines(True);result={};i=0
 while i<len(lines):
  assert lines[i].startswith('--- a/'),(name,i,lines[i]);path=lines[i][6:].strip();i+=1
  assert lines[i]=='+++ b/'+path+'\n';i+=1
  original=(D/'base'/path).read_text().splitlines(True);cursor=0;out=[]
  while i<len(lines) and not lines[i].startswith('--- a/'):
   m=re.fullmatch(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\n',lines[i]);assert m,(name,path,i,lines[i])
   old_start=int(m[1])-1;old_count=int(m[2] or 1);new_start=int(m[3])-1;new_count=int(m[4] or 1);i+=1
   assert old_start>=cursor;out+=original[cursor:old_start];cursor=old_start;assert len(out)==new_start
   old_seen=new_seen=0
   while i<len(lines) and not lines[i].startswith(('@@ ','--- a/')):
    line=lines[i];i+=1;assert line[:1] in (' ','-','+'),line
    if line[0] in (' ','-'):
     assert original[cursor]==line[1:],(name,path,cursor);cursor+=1;old_seen+=1
    if line[0] in (' ','+'):
     out.append(line[1:]);new_seen+=1
   assert (old_seen,new_seen)==(old_count,new_count),(name,path,old_seen,new_seen,old_count,new_count)
  out+=original[cursor:];result[path]=''.join(out).encode()
 return result
old=apply_patch('candidate-v3.patch');new=apply_patch('candidate-v4.patch');checks=[];records=[]
for row in base['files']:
 path=row['path'];a=D/'base'/path;p=D/'preview'/path
 blob=subprocess.run(['git','show',sha+':'+path],cwd=repo,check=True,stdout=subprocess.PIPE,timeout=20).stdout
 assert a.read_bytes()==blob and h(blob)==row['sha256']
 current=p.read_bytes();expected=new.get(path,blob);assert current==expected
 records.append({'path':path,'base_blob':row['git_blob'],'base_sha256':h(blob),'preview_path':str(p),'bytes':len(current),'mode':oct(stat.S_IMODE(p.stat().st_mode)),'sha256':h(current),'changed':current!=blob})
 checks.append({'path':path,'immutable_base_matches':True,'strict_patch_application_matches_preview':True})
correction=''
for path in sorted(set(old)|set(new)):
 a=old.get(path,(D/'base'/path).read_bytes()).decode();b=new.get(path,(D/'base'/path).read_bytes()).decode()
 correction+=''.join(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='v3/'+path,tofile='v4/'+path))
(D/'v3-to-v4.patch').write_text(correction)
manifest={'base_repository':repo,'base_commit':sha,'base_tree':subprocess.check_output(['git','rev-parse',sha+'^{tree}'],cwd=repo,text=True).strip(),'scope':'owned source previews only; uncompiled, unexecuted, unlanded','patch':{'path':str(D/'candidate-v4.patch'),'bytes':(D/'candidate-v4.patch').stat().st_size,'sha256':h((D/'candidate-v4.patch').read_bytes())},'files':records}
(D/'PREVIEW-MANIFEST-v4.json').write_text(json.dumps(manifest,indent=2)+'\n')
(D/'INTEGRITY-v4.json').write_text(json.dumps({'checks':checks,'changed_files':len(new),'v3_patch_preserved_sha256':h((D/'candidate-v3.patch').read_bytes()),'v4_patch_matches_all_previews':True,'v3_to_v4_patch_sha256':h(correction.encode()),'method':'strict in-memory unified-diff application with exact hunk counts/context and immutable Git blob comparison; no product execution or index use'},indent=2)+'\n')
# These are source declarations, not compiler-generated or executed inventory.
pat=re.compile(r'(?m)^[ \t]*#\[(?:test|tokio::test(?:[^\n]*))\]\s*(?:#\[[^\n]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)')
testrows=[]
for path in sorted(new):
 a=(D/'base'/path).read_text();b=(D/'preview'/path).read_text();at=[m[1] for m in pat.finditer(a)];bt=[m[1] for m in pat.finditer(b)]
 if at or bt:testrows.append({'path':path,'base_test_declarations':at,'preview_test_declarations':bt,'removed_or_renamed_names':sorted(set(at)-set(bt)),'added_or_renamed_names':sorted(set(bt)-set(at))})
(D/'AFFECTED-SOURCE-TEST-NAMES-v4.json').write_text(json.dumps({'scope':'test declarations in affected files, extracted from source only; no actual inventory, selection or execution claim','files':testrows},indent=2)+'\n')
print(json.dumps({'changed_files':len(new),'preview_entries':len(records),'patch_sha256':manifest['patch']['sha256'],'manifest_sha256':h((D/'PREVIEW-MANIFEST-v4.json').read_bytes()),'correction_sha256':h(correction.encode()),'correction_lines':len(correction.splitlines()),'source_test_name_changes':[{k:v for k,v in r.items() if k in ('path','removed_or_renamed_names','added_or_renamed_names')} for r in testrows]},indent=2))
