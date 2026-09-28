from pathlib import Path
import hashlib,json,re,difflib,subprocess,stat
D=Path(__file__).resolve().parent; B=json.loads((D/'BASE-v8.json').read_text());R=B['repository'];C=B['commit']
h=lambda b:hashlib.sha256(b).hexdigest()
def put(name,obj):
 data=obj if isinstance(obj,bytes) else (json.dumps(obj,indent=2)+'\n').encode()
 with (D/name).open('xb') as f:f.write(data)
def bind(p):
 p=Path(p);data=p.read_bytes();return {'path':str(p),'bytes':len(data),'sha256':h(data)}
for version in [5,6,7,8]:
 for row in json.loads((D/f'PACKET-v{version}.json').read_text())['inputs']:
  p=Path(row['path']);assert p.stat().st_size==row['bytes'] and h(p.read_bytes())==row['sha256'],p
for row in json.loads((D/'PREVIEW-MANIFEST-v8.json').read_text())['files']:
 p=Path(row['preview_path']);assert p.stat().st_size==row['bytes'] and h(p.read_bytes())==row['sha256'],p
for path in ['ci/configure-build-jobs.sh','scripts/lib/validate_cell_results.rs','tests/qemu-boot/strict_l2_test.sh']:
 data=subprocess.check_output(['git','show',C+':'+path],cwd=R,timeout=20)
 blob=subprocess.check_output(['git','rev-parse',C+':'+path],cwd=R,text=True,timeout=20).strip()
 B['files'].append({'path':path,'exists':True,'git_blob':blob,'bytes':len(data),'sha256':h(data)})
B['files'].sort(key=lambda r:r['path']);base={};old={};new={};records=[]
for row in B['files']:
 path=row['path']
 if row['exists']:
  data=subprocess.check_output(['git','show',C+':'+path],cwd=R,timeout=20);assert h(data)==row['sha256'] and len(data)==row['bytes']
 else:data=b''
 base[path]=data;p8=D/'preview-v8'/path;old[path]=p8.read_bytes() if p8.exists() else data
 p=D/'preview-v9'/path;new[path]=p.read_bytes()
 records.append({'path':path,'base_exists':row['exists'],'base_blob':row['git_blob'],'base_sha256':row['sha256'],'preview_path':str(p),'bytes':len(new[path]),'mode':oct(stat.S_IMODE(p.stat().st_mode)),'sha256':h(new[path]),'changed':new[path]!=data})
changes=sorted(p for p in new if new[p]!=old[p]);assert changes==['ci/configure-build-jobs.sh','hermit-cli/tests/verification_report_consumers.rs','scripts/lib/validate_cell_results.rs','tests/e2e/lib/applications/common.sh','tests/qemu-boot/strict_l2_test.sh'],changes
path='scripts/lib/validate_cell_results.rs';s=new[path].decode();start=s.index('    #[test]\n    fn matched_projection_rejects_unequal_counts_and_retains_sticky_divergence()');end=s.index('    #[test]\n    fn schema7_binds_virtual_time_to_the_comparison_mode()',start);s=s[:start]+s[end:]
s=s.replace('        let matched = report.require_canonical_match().is_ok();','        let matched = report.verified\n            && report.verdict == VerificationVerdict::Matched\n            && report.bitwise_parity;');assert s.encode()==old[path]
path='tests/qemu-boot/strict_l2_test.sh';assert new[path].replace(b'"$VERIFICATION_REPORT_BIN" canonical-match "$verify_report"',b'"$VERIFICATION_REPORT_BIN" matched "$verify_report"')==old[path]
path='ci/configure-build-jobs.sh';assert new[path].replace(b'# 526c21cf: build.rs blob',b'# 30fee360: build.rs blob')==old[path]
path='tests/e2e/lib/applications/common.sh';assert new[path].replace(b'# reader, which checks the current report, canonical strictness/envelope,\n# log comparison, positive equal counts, and the verified/matched/parity claim.',b'# reader, which rejects contradictory, incomplete, or filtered evidence.')==old[path]
path='hermit-cli/tests/verification_report_consumers.rs';s=new[path].decode();start=s.index('#[test]\nfn qemu_boot_consumer_rejects_noncanonical_and_unequal_match_claims()');end=s.index('#[test]\nfn json_output_retains_failure_evidence_without_satisfying_a_match_requirement()',start);s=s[:start]+s[end:]
start=s.index('        path: "tests/qemu-boot/strict_l2_test.sh",');end=s.index('        minimum_invocations:',start);s=s[:start]+s[start:end].replace('canonical-match','matched')+s[end:];assert s.encode()==old[path]
def diff(earlier,later,op,np,full=False):
 out=''
 for p in sorted(later):
  if earlier[p]==later[p]:continue
  oldname='/dev/null' if full and not next(r['exists'] for r in B['files'] if r['path']==p) else op+'/'+p
  out+=''.join(difflib.unified_diff(earlier[p].decode().splitlines(True),later[p].decode().splitlines(True),fromfile=oldname,tofile=np+'/'+p))
 return out.encode()
full=diff(base,new,'a','b',True);delta=diff(old,new,'v8','v9');put('candidate-v9.patch',full);put('v8-to-v9.patch',delta)
def apply(data,source,op,np):
 lines=data.decode().splitlines(True);out=dict(source);i=0
 while i<len(lines):
  assert lines[i].startswith('--- ');oldname=lines[i][4:].rstrip('\n');i+=1
  assert lines[i].startswith('+++ '+np+'/');path=lines[i][len(np)+5:].rstrip('\n');i+=1;assert oldname in ('/dev/null',op+'/'+path)
  original=source[path].decode().splitlines(True);cursor=0;result=[]
  while i<len(lines) and not lines[i].startswith('--- '):
   m=re.fullmatch(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\n',lines[i]);assert m,(path,i,lines[i]);i+=1
   old_n=int(m[2] or 1);new_n=int(m[4] or 1);old_start=int(m[1])-(1 if old_n else 0);new_start=int(m[3])-(1 if new_n else 0)
   assert old_start>=cursor;result+=original[cursor:old_start];cursor=old_start;assert len(result)==new_start
   a=b=0
   while i<len(lines) and not lines[i].startswith(('@@ ','--- ')):
    line=lines[i];i+=1;assert line[:1] in (' ','-','+')
    if line[0] in (' ','-'):assert cursor<len(original) and original[cursor]==line[1:],(path,cursor,line);cursor+=1;a+=1
    if line[0] in (' ','+'):result.append(line[1:]);b+=1
   assert (a,b)==(old_n,new_n)
  result+=original[cursor:];out[path]=''.join(result).encode()
 return out
assert apply(full,base,'a','b')==new and apply(delta,old,'v8','v9')==new
manifest={'base_repository':R,'base_commit':C,'base_tree':subprocess.check_output(['git','rev-parse',C+'^{tree}'],cwd=R,text=True,timeout=20).strip(),'scope':'owned source previews only; uncompiled, unexecuted, unlanded','patch':bind(D/'candidate-v9.patch'),'files':records}
put('BASE-v9.json',B);put('PREVIEW-MANIFEST-v9.json',manifest)
pat=re.compile(r'(?m)^[ \t]*#\[(?:test|tokio::test(?:[^\n]*))\]\s*(?:#\[[^\n]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)')
tests=[]
for path in changes:
 a=[m[1] for m in pat.finditer(old[path].decode())];b=[m[1] for m in pat.finditer(new[path].decode())];assert not set(a)-set(b)
 if a or b:tests.append({'path':path,'v8_or_immutable_base_declarations':a,'v9_declarations':b,'removed_names':[],'added_names':sorted(set(b)-set(a))})
put('AFFECTED-SOURCE-TEST-NAMES-v9.json',{'scope':'source declarations only; not emitted inventory or execution counts','inherited':bind(D/'AFFECTED-SOURCE-TEST-NAMES-v8.json'),'files':tests,'ledger_test_owner':'scripts/validate.rs rust-script test harness via validate_cell_results module; not manifest-plan Cargo tests'})
put('INTEGRITY-v9.json',{'method':'strict complete/delta patch application in memory with exact context/counts and immutable Git source checks; no product execution','materialized_files':len(new),'changed_paths':[r['path'] for r in records if r['changed']],'v8_to_v9_changed_paths':changes,'complete_and_increment_apply_exactly':True,'all_v5_v6_v7_v8_packets_and_v8_preview_preserved':True,'only_two_consumer_decisions_two_new_test_methods_and_two_comment_corrections':True,'all_previous_consumer_table_entries_retained':True,'ledger_canonical_report_and_all_comparison_policy_divergence_logic_unchanged':True,'qemu_boot_argv_marker_clock_status_phase_bounds_unchanged':True,'all_v8_core_comparator_report_and_other_consumer_implementation_bytes_unchanged':True,'sdk_pins_keys_limits_and_executable_bytes_unchanged':True,'actual_inventory_or_product_execution':False})
for p in ['candidate-v9.patch','v8-to-v9.patch','PREVIEW-MANIFEST-v9.json']:print(json.dumps(bind(D/p)))
print(json.dumps({'changed_paths':sum(r['changed'] for r in records),'materialized_files':len(records)}))
