from pathlib import Path
import hashlib,json
D=Path(__file__).resolve().parent;O=D/'frozen-v3'
def rec(p):
 p=Path(p);b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest())
records=[];names={'lib':set(),'static':set()};total_cpu=0;total_payload=0
for qname in ['negative-before-v1','positive-v1','negative-before-v2','positive-v2','positive-v2-continuation']:
 q=D/qname
 for p in sorted((q/'controls').glob('*/result.json')):
  r=json.loads(p.read_text());name=p.parent.name;op=Path(r['observer_result']['path']);o=json.loads(op.read_text());account=o.get('final_accounting') or o.get('refusal_final_accounting');cpu=account['cpu_usage_nsec']/1e9 if account else None;payload=r.get('payload_exit',{}).get('elapsed_seconds')
  logs=[rec(q/'observer'/name/s)for s in ['stdout','stderr','payload-exit.json','before-exec-receipt.json']if(q/'observer'/name/s).is_file()]
  row=dict(qualification=qname,phase=name,result=rec(p),plan=rec(q/(name+'-plan.json')),observer=rec(op),raw=logs,accepted=r['accepted'],raw_status=r.get('raw_status'),terminal_authenticated=r['terminal_authenticated'],inputs_unchanged=r.get('inputs_unchanged'),aggregate_cpu_seconds=cpu,payload_seconds=payload,observer_seconds=o.get('elapsed_seconds'),readback=r.get('readback'),failure_scope='Historical original/intermediate failure, not silently replaced' if not r['accepted']else 'Actual qualified component evidence')
  records.append(row)
  if qname=='positive-v2' and name.startswith('test-'):
   assert r['accepted']and r['terminal_authenticated']and r['inputs_unchanged']and r['raw_status']==0
   artifact=json.loads((q/'SELECTORS.json').read_text())['groups'][name]['artifact'];ns=r['readback']['names'];assert not(names[artifact]&set(ns));names[artifact].update(ns);total_cpu+=cpu;total_payload+=payload
assert[len(names[x])for x in ['lib','static']]==[49,13]
retained=[]
for name in ['negative-before-v1','positive-v1','negative-before-v2','positive-v2']:
 p=D/name/'RETAINED_ELFS.json';v=json.loads(p.read_text());retained.append(rec(p))
 # Retained ELF copy identity, never the historical mutable compiler output path.
 def visit(x):
  if isinstance(x,dict):
   if 'path' in x and ('retained' in x['path'] or '/artifacts/' in x['path']) and x['path'].endswith('.elf'):
    assert rec(x['path'])['sha256']==x['sha256']
   for z in x.values():visit(z)
  elif isinstance(x,list):
   for z in x:visit(z)
 visit(v)
result=dict(scope='Finite Reverie component only, no full workspace/DAG, Hermit timers or Linux nonleader-exec support claim',unique_test_names={k:sorted(v)for k,v in names.items()},unique_counts={k:len(v)for k,v in names.items()},sum_qualified_test_aggregate_cpu_seconds=total_cpu,sum_qualified_test_payload_seconds=total_payload,records=records,retained_elf_bindings=retained,qualified_source_manifest=rec(D/'positive-v2/source-manifest.json'),continuation_source_manifest=rec(D/'positive-v2-continuation/source-manifest.json'),startup_recovery=[rec(D/'format-startup-recovery-v1'/n)for n in ['PROOF.json','COMPLETION.json','CONTROLS.json','CONTINUATION-LEASE.patch']],limits=dict(compile_check_cpu_seconds=600,compile_check_wall_seconds=900,test_format_cpu_seconds=30,test_format_wall_seconds=60,memory_bytes=17179869184,swap_bytes=0,stderr_bytes=16777216,stdout_bytes=67108864,free_floor_bytes=107374182400,cargo_jobs=2),toolchain='nightly-2026-07-29; exact executables, stdlib/dependency closure, loader, source and SCM bound by each plan',historical_mutable_paths='Compiler output paths in old plans are provenance, not current identities. Use retained_elf_bindings for old executed ELF bytes; those readonly copies were checked at freeze.')
(O/'QUALIFICATION.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(dict(qualification=rec(O/'QUALIFICATION.json'),test_cpu=total_cpu,test_payload=total_payload),indent=2))
