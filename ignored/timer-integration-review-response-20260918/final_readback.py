import hashlib,json,os,subprocess,sys
from pathlib import Path
repo=Path(__file__).resolve().parents[2];own=Path(__file__).resolve().parent;f=own/'frozen-v2';q=own/'qualification-v6'
def rec(p):
 p=Path(p).resolve();raw=p.read_bytes();return dict(path=str(p),bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest())
def check(r):
 actual=rec(r['path']);assert actual['bytes']==r['bytes'] and actual['sha256']==r['sha256'],r['path']
s=json.loads((f/'SOURCE_INPUTS.json').read_text());pres=json.loads((f/'PREDECESSOR_PRESERVATION.json').read_text())
for r in pres['authenticated']:check(r)
for e in s['changed_paths']:
 for k in ['before','after','predecessor','working']:
  if isinstance(e.get(k),dict):check(e[k])
for e in s['context']:check(e['snapshot']);check(e['working'])
for e in s['design_inputs']+s['design_reviews']+s['source_reviews']:
 if {'path','bytes','sha256'} <= e.keys():check(e)
check(s['complete_source_manifest']);check(s['scm']['index_raw'])
sys.path.insert(0,str(q));import phase
for e in json.loads((f/'QUALIFICATION.json').read_text())['phases']:
 check(e['result']);check(e['plan']);check(e['observer']);check(e['stdout']);check(e['stderr'])
 phase.check(json.loads(Path(e['plan']['path']).read_text()))
for e in json.loads((f/'ARTIFACTS.json').read_text()).values():check(e['original']);check(e['frozen'])
for patch in ['SOURCE.patch','DELTA.patch']:
 subprocess.run(['/usr/bin/git','-C',str(repo),'apply','--reverse','--check',str(f/patch)],check=True)
env={**os.environ,'GIT_OPTIONAL_LOCKS':'0','GIT_NO_LAZY_FETCH':'1'}
head=subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'],env=env).decode().strip();assert head==s['base']
branch=subprocess.check_output(['git','-C',str(repo),'branch','--show-current'],env=env).decode().strip();assert branch==s['scm']['branch']
index=subprocess.check_output(['git','-C',str(repo),'ls-files','--stage'],env=env);assert hashlib.sha256(index).hexdigest()==s['scm']['index_entries_sha256']
subprocess.run(['git','-C',str(repo),'diff','--cached','--quiet'],env=env,check=True)
records=[rec(p) for p in sorted(f.iterdir()) if p.is_file()]
readback=dict(schema=2,source_and_scm_unchanged=True,all_final_phase_inputs_reauthenticated=True,reverse_full_and_delta_patch_checks=True,predecessor_inputs_unchanged=True,changed_paths=len(s['changed_paths']),delta_paths=len(s['delta_paths']),context_snapshots=len(s['context']),full_product_records=s['complete_source_records'],records=records,final_tests=dict(library_passed=36,vm_passed=7,failed=0,ignored=0,library_inventory=478,static_inventory=294),negative_controls=[dict(qualification='qualification-v1',findings=['native R1 cross-lifetime equality','native R2 replay removal','native R3 caught action mutation']),dict(qualification='qualification-v5',findings=['Claude F1 wrong sibling unit','Claude F1 wrong sibling actual VM','Claude F2 unrelated getpid EOVERFLOW actual VM'])],original_failures_retained=True,cleanup_recovery_does_not_qualify_original_failure=True,raw_index_drift_preserved=True,ownership='Sole source/SCM ownership transferred back to root at final handoff; no further source writes absent explicit reassignment.',review_status='Author candidate; independent native/Claude reviews and integrated runtime qualification pending.',source_only_changes_after_final_tests=False)
p=f/'READBACK.json';assert not p.exists();p.write_text(json.dumps(readback,indent=2)+'\n')
for r in records:check(r)
print(json.dumps({n:rec(f/n) for n in ['SOURCE.patch','DELTA.patch','SOURCE_INPUTS.json','REPORT.md','READBACK.json','API_INVENTORY.json','QUALIFICATION.json']},indent=2))
