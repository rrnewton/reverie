from pathlib import Path, PurePosixPath
import json, hashlib, re, collections, shutil
P=Path(__file__).resolve().parent
D=Path('/home/newton/work/dev-hermit/ignored/validate/artifacts/validate-cleanup-3-80ce2ee94689-1789724738166435665-509588-1a3c3ec7/e2e')
F=D/'portable/manifest_backend_parity_c/results.jsonl'
E=P/'evidence/parity'; E.mkdir(parents=True,exist_ok=False)
h=lambda b:hashlib.sha256(b).hexdigest()
def save(origin,dest,expected=None):
 b=origin.read_bytes()
 if expected is not None:assert h(b)==expected,(origin,h(b),expected)
 dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(b)
 assert dest.read_bytes()==b
 return {'source':str(origin),'path':str(dest),'bytes':len(b),'sha256':h(b)}
b=F.read_bytes(); assert len(b)==11300714 and h(b)=='b02421c8c0136fdb6f3444462c6990851f03d09af7adc1a407dc92a3fb3a743c'
inputs=[save(F,E/'results.jsonl',h(b))]; rows=[json.loads(x) for x in b.splitlines()]
index=[]; counts=collections.Counter()
pattern=re.compile(rb' INFO detcore: \[dtid [0-9]+\] inbound rdtsc,')
for line,row in enumerate(rows,1):
 if row['backend']!='kvm':continue
 relative=PurePosixPath(row['artifact_dir']).relative_to('/results'); origin=D/relative
 cell={'line':line,'test':row['test'],'attempt':row['attempt'],'outcome':row['outcome'],'reason':row['reason'], 'artifact_dir':row['artifact_dir'],'actual_artifact_dir':str(origin),'row_test_sha256':row['test_sha256'],'argv':row['argv'],'effective_args':row['effective_args'],'guest_argv':row['guest_argv'],'env':row['env'],'cwd':row['cwd'],'execution_cpu_timeout_seconds':row['execution_cpu_timeout_seconds'],'execution_wall_timeout_seconds':row['execution_wall_timeout_seconds'],'relaxations':row['relaxations'],'backend_parity':row['backend_parity'],'logs':{},'attempt_commands':[{k:a.get(k) for k in ('index','status','signal','timed_out','outcome','argv','guest_argv','env','cwd','verification_report_sha256')} for a in row['attempts']]}
 for side in ('reference','candidate'):
  report=row['backend_parity'][side]; f=origin/report['retained_log'];dest=E/'artifacts'/relative.name/report['retained_log']; record=save(f,dest,report['retained_log_sha256']);inputs.append(record)
  raw=dest.read_bytes();lines=raw.splitlines();events=[{'line':i,'text':x.decode()} for i,x in enumerate(lines,1) if pattern.search(x)]
  cell['logs'][side]={'record':record,'timestamp_events':events,'timestamp_count':len(events)}
 f=origin/'fixtures/program';record=save(f,E/'artifacts'/relative.name/'fixtures/program');inputs.append(record);cell['fixture_elf']=record
 for name in ('backend-parity.json','backend-parity-logdiff.json','verify-1.json','verify-parity-reference.json','captures/verify-1.stdout','captures/verify-1.stderr','captures/verify-parity-reference.stdout','captures/verify-parity-reference.stderr'):
  f=origin/name
  if f.is_file():inputs.append(save(f,E/'artifacts'/relative.name/name))
 counts[(cell['logs']['reference']['timestamp_count'],cell['logs']['candidate']['timestamp_count'])]+=1
 cell['timestamp_signature']=('inbound rdtsc,' in (row['first_divergent_left_message'] or '') and cell['logs']['reference']['timestamp_count']>0 and cell['logs']['candidate']['timestamp_count']==0)
 index.append(cell)
matched=sorted({x['test'] for x in index if x['timestamp_signature']});other=sorted({x['test'] for x in index if not x['timestamp_signature']})
summary={'kind':'read-only authentication and indexing of historical actual results; not candidate qualification','source_rows':len(rows),'kvm_rows':len(index),'unique_kvm_cells':len({x['test'] for x in index}),'timestamp_signature_cells':len(matched),'matched_cells':matched,'other_cells':other,'attempt_count_distribution':dict(collections.Counter(x['attempt'] for x in index)),'actual_timestamp_pair_counts':[{'reference':a,'candidate':b,'rows':n} for (a,b),n in sorted(counts.items())],'twenty_event_cells':sorted({x['test'] for x in index if x['logs']['reference']['timestamp_count']==20}),'all_kvm_parity_failures_preserved':all(x['outcome']=='FAIL' for x in index),'all_input_hashes_authenticated':True,'no_comparator_or_selected_event_change':True}
for name,obj in [('KVM-ROWS.json',index),('INPUTS.json',inputs),('SUMMARY.json',summary)]:
 (E/name).write_text(json.dumps(obj,indent=2)+'\n')
print(json.dumps({k:v for k,v in summary.items() if k!='matched_cells'},indent=2))
print('retained inputs',len(inputs),'bytes',sum(x['bytes'] for x in inputs))
