from pathlib import Path
import hashlib,json,re,difflib,subprocess,stat
D=Path(__file__).resolve().parent
B=json.loads((D/'BASE-v7.json').read_text()); R=B['repository']; C=B['commit']
h=lambda b:hashlib.sha256(b).hexdigest()
def put(name,obj):
 data=obj if isinstance(obj,bytes) else (json.dumps(obj,indent=2)+'\n').encode()
 with (D/name).open('xb') as f:f.write(data)
def bind(p):
 p=Path(p);data=p.read_bytes();return {'path':str(p),'bytes':len(data),'sha256':h(data)}
for version in [5,6,7]:
 for row in json.loads((D/f'PACKET-v{version}.json').read_text())['inputs']:
  p=Path(row['path']);assert p.stat().st_size==row['bytes'] and h(p.read_bytes())==row['sha256'],p
for row in json.loads((D/'PREVIEW-MANIFEST-v7.json').read_text())['files']:
 p=Path(row['preview_path']);assert p.stat().st_size==row['bytes'] and h(p.read_bytes())==row['sha256'],p
base={};old={};new={};records=[]
for row in B['files']:
 path=row['path']
 if row['exists']:
  data=subprocess.check_output(['git','show',C+':'+path],cwd=R,timeout=20)
  assert h(data)==row['sha256'] and len(data)==row['bytes']
 else:data=b''
 base[path]=data;old[path]=(D/'preview-v7'/path).read_bytes();new[path]=(D/'preview-v8'/path).read_bytes()
 p=D/'preview-v8'/path
 records.append({'path':path,'base_exists':row['exists'],'base_blob':row['git_blob'],'base_sha256':row['sha256'],'preview_path':str(p),'bytes':len(new[path]),'mode':oct(stat.S_IMODE(p.stat().st_mode)),'sha256':h(new[path]),'changed':new[path]!=data})
changes=sorted(p for p in new if new[p]!=old[p]);assert changes==['hermit-cli/src/canonical_verdict.rs','hermit-cli/tests/verification_report_consumers.rs'],changes
canonical='hermit-cli/src/canonical_verdict.rs';s=new[canonical].decode()
start=s.index('            if !self\n',s.index('    pub fn require_canonical_match'))
end=s.index('            Ok(())',start);s=s[:start]+s[end:]
start=s.index('    #[test]\n    fn canonical_match_rejects_unequal_counts_without_rewriting_history()')
end=s.index('    /// The exact bytes hermit writes',start);s=s[:start]+s[end:]
assert s.encode()==old[canonical], 'canonical delta exceeds one check and one test'
tests='hermit-cli/tests/verification_report_consumers.rs';s=new[tests].decode()
start=s.index('#[test]\nfn canonical_reader_rejects_unequal_counts_while_matched_and_inspection_stay_distinct()')
end=s.index('#[test]\nfn reproducible_build_consumer_keeps_artifact_and_typed_verdict_requirements()',start);s=s[:start]+s[end:]
assert s.encode()==old[tests], 'consumer delta exceeds one test'
def diff(earlier,later,oldprefix,newprefix,full=False):
 output=''
 for p in sorted(later):
  if earlier[p]==later[p]:continue
  oldname='/dev/null' if full and not next(row['exists'] for row in B['files'] if row['path']==p) else oldprefix+'/'+p
  output+=''.join(difflib.unified_diff(earlier[p].decode().splitlines(True),later[p].decode().splitlines(True),fromfile=oldname,tofile=newprefix+'/'+p))
 return output.encode()
full=diff(base,new,'a','b',True);delta=diff(old,new,'v7','v8')
put('candidate-v8.patch',full);put('v7-to-v8.patch',delta)
def apply(data,source,oldprefix,newprefix):
 lines=data.decode().splitlines(True);out=dict(source);i=0;applied=[]
 while i<len(lines):
  assert lines[i].startswith('--- ');oldname=lines[i][4:].rstrip('\n');i+=1
  assert lines[i].startswith('+++ '+newprefix+'/');path=lines[i][len(newprefix)+5:].rstrip('\n');i+=1
  assert oldname in ('/dev/null',oldprefix+'/'+path)
  original=source[path].decode().splitlines(True);cursor=0;result=[]
  while i<len(lines) and not lines[i].startswith('--- '):
   m=re.fullmatch(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\n',lines[i]);assert m,(path,i,lines[i]);i+=1
   old_n=int(m[2] or 1);new_n=int(m[4] or 1)
   old_start=int(m[1])-(1 if old_n else 0);new_start=int(m[3])-(1 if new_n else 0)
   assert old_start>=cursor;result+=original[cursor:old_start];cursor=old_start;assert len(result)==new_start
   old_seen=new_seen=0
   while i<len(lines) and not lines[i].startswith(('@@ ','--- ')):
    line=lines[i];i+=1;assert line[:1] in (' ','-','+')
    if line[0] in (' ','-'):
     assert cursor<len(original) and original[cursor]==line[1:],(path,cursor,line);cursor+=1;old_seen+=1
    if line[0] in (' ','+'):
     result.append(line[1:]);new_seen+=1
   assert (old_seen,new_seen)==(old_n,new_n)
  result+=original[cursor:];out[path]=''.join(result).encode();applied.append(path)
 return out,applied
for patch,prior,op,np in [(full,base,'a','b'),(delta,old,'v7','v8')]:
 result,paths=apply(patch,prior,op,np);assert result==new
manifest={'base_repository':R,'base_commit':C,'base_tree':subprocess.check_output(['git','rev-parse',C+'^{tree}'],cwd=R,text=True,timeout=20).strip(),'scope':'owned source previews only; uncompiled, unexecuted, unlanded','patch':bind(D/'candidate-v8.patch'),'files':records}
put('BASE-v8.json',B);put('PREVIEW-MANIFEST-v8.json',manifest)
pat=re.compile(r'(?m)^[ \t]*#\[(?:test|tokio::test(?:[^\n]*))\]\s*(?:#\[[^\n]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)')
testrows=[]
for path in changes:
 a=[m[1] for m in pat.finditer(old[path].decode())];b=[m[1] for m in pat.finditer(new[path].decode())]
 assert not set(a)-set(b)
 testrows.append({'path':path,'v7_declarations':a,'v8_declarations':b,'removed_names':[],'added_names':sorted(set(b)-set(a))})
put('AFFECTED-SOURCE-TEST-NAMES-v8.json',{'scope':'source declarations only; no compiler emitted inventory or runtime counts','inherited':bind(D/'AFFECTED-SOURCE-TEST-NAMES-v7.json'),'files':testrows})
put('INTEGRITY-v8.json',{'method':'strict in-memory complete and incremental unified-diff application with exact hunk context/counts; immutable Git blob and frozen v7 byte checks; no product execution','materialized_files':len(new),'changed_paths':[r['path'] for r in records if r['changed']],'v7_to_v8_changed_paths':changes,'complete_and_increment_apply_exactly':True,'all_v5_v6_v7_packet_inputs_preserved':True,'all_v7_preview_bytes_preserved':True,'v8_changes_only_match_count_equality_and_two_new_tests':True,'historical_parser_and_canonical_comparison_bytes_unchanged':True,'all_other_v7_implementation_and_fixture_bytes_unchanged':True,'original_test_names_and_assertions_retained':True,'actual_inventory_or_product_execution':False})
print(json.dumps({'patch':bind(D/'candidate-v8.patch'),'delta':bind(D/'v7-to-v8.patch'),'manifest':bind(D/'PREVIEW-MANIFEST-v8.json'),'changed_paths':sum(r['changed'] for r in records),'materialized_files':len(new)},indent=2))
