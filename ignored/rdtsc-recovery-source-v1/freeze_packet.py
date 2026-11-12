from pathlib import Path
import os,json,hashlib,stat,ast
P=Path(__file__).resolve().parent
sha=lambda b:hashlib.sha256(b).hexdigest()
def rec(p):
 st=p.lstat();r={'path':str(p),'mode':stat.S_IMODE(st.st_mode)}
 if stat.S_ISLNK(st.st_mode):
  target=os.readlink(p);r.update(kind='symlink',target=target,bytes=len(os.fsencode(target)),sha256=sha(os.fsencode(target)))
 else:
  assert stat.S_ISREG(st.st_mode),p;b=p.read_bytes();r.update(kind='file',bytes=len(b),sha256=sha(b))
 return r
def write(name,obj):(P/name).write_text(json.dumps(obj,indent=2)+'\n')
# No author helper is imported or executed. Parse only the intended executable Python syntax.
for p in (P/'qualification-v1').glob('*.py'):ast.parse(p.read_text(),filename=str(p))
manifest=json.loads((P/'SOURCE-MANIFEST.json').read_text())
for row in manifest:
 p=Path(row['path'])
 if row['kind']=='unexpanded_gitlink':assert p.is_dir() and not p.is_symlink() and not list(p.iterdir())
 elif row['kind']=='symlink':assert p.is_symlink() and sha(os.fsencode(os.readlink(p)))==row['sha256']
 else:assert rec(p)['sha256']==row['sha256'] and rec(p)['bytes']==row['bytes'] and rec(p)['mode']==row['file_mode']
# A copied historical report retains its original text, including old nonblocking assessments.
comments=json.loads((P/'evidence/kvm-rdtsc-prior-pr403-comments-20260918.json').read_text())
def walk(x):
 if isinstance(x,dict):
  if str(x.get('id'))=='5248210234' and 'body'in x:return x['body']
  for y in x.values():
   z=walk(y)
   if z is not None:return z
 elif isinstance(x,list):
  for y in x:
   z=walk(y)
   if z is not None:return z
 return None
body=walk(comments);assert body is not None
(P/'evidence/PR403-BLOCKER.md').write_text(body)
mandatory=['REPORT.md','PLAN.md','CALLER-PLAN.md','SOURCE-MAP.md','SOURCE.patch','CALLER.patch','CONTROLS.json','HISTORICAL-CONTROL-CONTINUITY.json','evidence/PR403-BLOCKER.md','evidence/parity/SUMMARY.json']
chunks=P/'chunks';chunks.mkdir();docs=[];serial=0
for name in mandatory:
 p=P/name;lines=p.read_bytes().splitlines(keepends=True);parts=[]
 for start in range(0,len(lines),60):
  serial+=1;dest=chunks/('%03d.txt'%serial);dest.write_bytes(b''.join(lines[start:start+60]));parts.append(dict(first_line=start+1,last_line=min(start+60,len(lines)),file=rec(dest)))
 assert b''.join(Path(c['file']['path']).read_bytes()for c in parts)==p.read_bytes()
 docs.append(dict(document=rec(p),lines=len(lines),chunks=parts))
write('COMPLETE_READS.json',{'status':'mandatory bounded review inventory; no external reviewer launched','documents':docs,'total_documents':len(docs),'total_chunks':serial,'total_lines':sum(d['lines']for d in docs),'full_source_context':'Every candidate source file is separately bound; required current exception/lifecycle/clock callers are identified by SOURCE-MAP.md.'})
unchanged=[]
orig=json.loads((P/'qualification-v1/RUNNER_ORIGINS.json').read_text())
for name in orig['protocol_unchanged_helpers']:
 before=next(x['original']for x in orig['sources']if x['original']['path'].endswith('/'+name));after=rec(P/'qualification-v1'/name);assert before['sha256']==after['sha256'];unchanged.append({'name':name,'original':before,'actual':after,'byte_identical':True})
write('CALLER-READBACK.json',{'status':'frozen prepare-only caller, not execution approval','material_delta':rec(P/'CALLER.patch'),'setup':rec(P/'qualification-v1/SETUP.json'),'selectors':rec(P/'qualification-v1/SELECTORS.json'),'source_manifest':rec(P/'qualification-v1/source-manifest.json'),'phase_count':39,'test_declarations':30,'observer_sha256':'137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179','unchanged_protocol_helpers':unchanged,'new_helpers':[rec(P/'qualification-v1'/x)for x in ('admit_target.py','retain_artifacts.py')],'target_creation_lease_claim_or_execution_performed':False,'plan_generation_pending_fresh_identity_and_authorization':True})
write('TARGET.json',{'status':'FROZEN SOURCE CANDIDATE AND PREPARE-ONLY QUALIFICATION CALLER; NO EXECUTION OR APPROVAL','scope':'CPL3 ELF Tool timestamp callback; public CPL0/real-mode gap remains','authorship_base':'79516661bf82d30ab2967c71834a6d47447b76ee','landed_tree_equivalent_base_reported_by_root':'44fcb1955f44547f50d724fe8f7d718215fed446','base_tree':'7620fe83f486d665d9d09d4f09f0e93636b862e4','source':str(P/'source'),'source_patch':rec(P/'SOURCE.patch'),'source_manifest':rec(P/'SOURCE-MANIFEST.json'),'report':rec(P/'REPORT.md'),'plan':rec(P/'PLAN.md'),'controls':rec(P/'CONTROLS.json'),'source_inputs':rec(P/'SOURCE_INPUTS.json'),'caller':rec(P/'CALLER-READBACK.json'),'complete_reads':rec(P/'COMPLETE_READS.json'),'same_run_cells':75,'timestamp_signature_cells':74,'all_prior_parity_failures_retained':True,'compile_test_guest_network_scm_performed':False,'source_formatting_only':'source-format/RESULT.json and source-format-v2/RESULT.json; no test credit'})
# Bind every retained leaf without following symbolic directory aliases. Include
# the extra ignored lock and all original evidence, not only changed files.
records=[]
for root,dirs,files in os.walk(P,followlinks=False):
 for name in list(dirs):
  p=Path(root)/name
  if p.is_symlink():records.append(rec(p));dirs.remove(name)
 for name in files:
  p=Path(root)/name
  if p.parent==P and name in ('INPUTS.json','READBACK.json'):continue
  records.append(rec(p))
records.sort(key=lambda x:x['path']);write('INPUTS.json',{'records':records,'leaf_count':len(records),'source_entries':len(manifest),'policy':'regular bytes/modes and literal symlink targets, no implicit gitlink expansion; all retained historical logs/results included'})
for r in records:assert rec(Path(r['path']))==r
write('READBACK.json',{'target':rec(P/'TARGET.json'),'inputs':rec(P/'INPUTS.json'),'all_retained_records_authenticated':True,'records':len(records),'source_entries':len(manifest),'changed_paths':7,'base_tree_authenticated_offline':True,'historical_static_bodies_byte_identical':6,'current_static_tests_unchanged':True,'original_kvm_rows':150,'original_cells':75,'actual_timestamp_signature_cells':74,'caller_phase_count':39,'planned_exact_declarations':30,'runtime_qualified':False,'independent_approval':False})
for name in ('TARGET.json','SOURCE.patch','SOURCE-MANIFEST.json','REPORT.md','PLAN.md','CALLER.patch','CALLER-READBACK.json','COMPLETE_READS.json','INPUTS.json','READBACK.json'):print(name,sha((P/name).read_bytes()),(P/name).stat().st_size)
print('mandatory coverage',len(docs),serial,sum(d['lines']for d in docs))
