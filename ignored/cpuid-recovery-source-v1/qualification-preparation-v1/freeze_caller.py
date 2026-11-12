from pathlib import Path
import hashlib,json,os,sys
Q=Path(__file__).resolve().parent;N=Q.parent;R=N.parents[1]
H=R.parent/'kvm-replay-prerequisites-20260918'
def rec(p):
 p=Path(p);st=p.lstat();b=os.fsencode(os.readlink(p)) if p.is_symlink() else p.read_bytes();d={'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest(),'mode':st.st_mode&0o7777}
 if p.is_symlink():d.update(kind='symlink',target=os.readlink(p))
 return d
def write(p,v):
 with Path(p).open('x')as f:f.write(v if isinstance(v,str)else json.dumps(v,indent=2)+'\n')
source=rec(N/'TARGET.json');assert source['sha256']=='5ff12f1d6800aaa5944efcbb431fd23d79740b16430ed1b960a9ced13619d3b8'
order=json.loads((N/'RUN-ORDER.json').read_text())['phases'];assert len(order)==64
steps=[
 {'step':'admit-new-target','argv':['/usr/bin/python3','-B',str(Q/'admit_target.py')],'requires':'root execution authorization; exact target must still be absent'},
 {'step':'metadata','argv':['/usr/bin/python3','-B',str(N/'execute_phases.py'),'metadata'],'requires':'successful retained fresh-target admission; existing lease admission performed by unchanged phase helper'},
 {'step':'bind-actual-dependencies','argv':['/usr/bin/python3','-B',str(Q/'bind_dependencies.py')],'requires':'accepted terminal/source-authenticated metadata receipt'},
 {'step':'compile','argv':['/usr/bin/python3','-B',str(N/'execute_phases.py'),'compile'],'requires':'actual external dependency closure bound'},
 {'step':'retain-four-fresh-harnesses','argv':['/usr/bin/python3','-B',str(Q/'retain_artifacts.py')],'requires':'accepted compile, no structured warnings, actual fresh=false and source-bound emitted artifacts'},
 {'step':'remaining-phases','argv':['/usr/bin/python3','-B',str(N/'execute_phases.py'),*order[2:]],'requires':'source-bound immutable copies and linked-library proof'},
 {'step':'read-actual-results','argv':['/usr/bin/python3','-B',str(N/'finalize_results.py')],'requires':'only after the phase driver stops; preserve any reader refusal instead of reconstructing receipts'}]
write(Q/'LAUNCH-PLAN.json',{'source_target':source,'cwd':str(N),'environment_override':{'PYTHONOPTIMIZE':'0'},'actual_optimize_debug_record_required_before_admission':True,'steps':steps,'stop_first_unaccepted':True,'no_retry':True,'phases':64,'declarations':55,'phase_execution_authorized':False})
# Later chronology is measured in a separate composition and cannot qualify this candidate.
C=H/'ignored/kvm-rpc-cancellation-h39ac-v1/measured-final-v1';bound=json.loads((C/'READBACK.json').read_text());assert rec(C/'READBACK.json')['sha256']=='f5a4a5ffd183ebc2824af46a27f93123c98964eb6419452d909a071c3bd4de29'
chron=[rec(C/'READBACK.json')]
for row in bound.values():
 actual=rec(row['path']);assert actual['sha256']==row['sha256']and actual['bytes']==row['bytes'];chron.append(actual)
case=next(c for c in json.loads((C/'RESULTS.json').read_text())['cases']if c['name']=='timestamp-cancel-v1');assert case['status']==17 and case['compared_info_messages']==409
for trace in case['traces']:
 actual=rec(trace['file']['path']);assert actual['sha256']==trace['file']['sha256']and actual['bytes']==trace['file']['bytes'];lines=Path(actual['path']).read_text().splitlines();assert len(lines)==trace['lines']
 for row in trace['timestamp_order']+trace['cleanup']:assert lines[row['line']-1]==row['text']
 assert trace['callbacks']==trace['charges']==372 and trace['before_charge_coverage']is False;chron.append(actual)
write(Q/'CHRONOLOGY-INPUTS.json',{'records':chron,'own_read_scope':'complete REPORT and READBACK; selected timestamp result; both whole trace hashes and exact 12 ordered/9 cleanup row matches per trace, not every trace line or a source verdict','coordinator_reported_hermit_elf':'a8408c776c850ed61ff984c7d61d09b4c0027fa6fd9cf9d492ad6983c75be8c3','before_charge_or_clock_rpc_credit':False,'CPUID_candidate_execution_credit':False})
# Existing implementation source packet remains byte-identical. Normalize
# this new caller input index without rewriting its frozen source index.
index=json.loads((Q/'CALLER-INPUTS.json').read_text());records={row['path']:row for row in index['records']}
for row in records.values():assert rec(row['path'])==row
for row in chron:records[row['path']]=row
for p in Q.rglob('*'):
 if p.is_file() and p.name not in ['CALLER-INPUTS.json','CALLER-TARGET.json','CALLER-READBACK.json']:records[str(p)]=rec(p)
index['records']=list(records.values());index['no_phase_execution']=True
(Q/'CALLER-INPUTS.json').write_text(json.dumps(index,indent=2)+'\n')
origins=json.loads((Q/'RUNNER_ORIGINS.json').read_text())['records'];identical=[];changed=[]
for row in origins:
 assert rec(row['before']['path'])==row['before']and rec(row['after']['path'])==row['after']
 assert (Path(row['before']['path']).read_bytes()==Path(row['after']['path']).read_bytes())==row['byte_identical']
 (identical if row['byte_identical']else changed).append(Path(row['after']['path']).name)
assert set(changed)=={'prepare.py','admit_target.py','execute_phases.py','finalize_results.py'}
assert rec(Q/'observer/observer.py')['sha256']=='137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179'
assert (Q/'SELECTORS.json').read_bytes()==(N/'SELECTORS.json').read_bytes()
actual_source=json.loads((N/'INPUTS.json').read_text())['records']
for row in actual_source:
 p=Path(row['path'])
 if row['kind']=='unexpanded_gitlink':assert p.is_dir()and not list(p.iterdir())
 else:assert rec(p)['sha256']==row['sha256']
write(Q/'CALLER-TARGET.json',{'status':'FROZEN PREPARE-ONLY CALLER; NO ADMISSION OR PHASE LAUNCHED','source_target':source,'source_unchanged':True,'source_manifest':rec(Q/'source-manifest.json'),'setup':rec(Q/'SETUP.json'),'caller_delta':rec(Q/'CALLER-DELTA.patch'),'plan':rec(Q/'CALLER-PLAN.md'),'launch_plan':rec(Q/'LAUNCH-PLAN.json'),'order':rec(N/'RUN-ORDER.json'),'selectors':rec(Q/'SELECTORS.json'),'origins':rec(Q/'RUNNER_ORIGINS.json'),'inputs':rec(Q/'CALLER-INPUTS.json'),'preparation_state':rec(Q/'PREPARATION-STATE.json'),'chronology_addendum':rec(Q/'CANCELLATION-CHRONOLOGY.md'),'chronology_inputs':rec(Q/'CHRONOLOGY-INPUTS.json'),'target_path':str(R/'target/cpuid-recovery-v1-cold-v1'),'phase_count':64,'declaration_count':55,'preserved_helper_algorithms':identical,'materially_changed_helper_paths':changed,'no_source_or_independent_approval':True})
for row in records.values():assert rec(row['path'])==row
write(Q/'CALLER-READBACK.json',{'target':rec(Q/'CALLER-TARGET.json'),'inputs':rec(Q/'CALLER-INPUTS.json'),'records_authenticated':len(records),'aliases':sum(row.get('kind')=='symlink'for row in records.values()),'frozen_source_records_unchanged':len(actual_source),'source_entries':2627,'exact_selectors_preserved':55,'planned_phases':64,'source_target':source,'observer_unchanged':True,'target_admitted':False,'lease_opened':False,'phase_launched':False,'python_optimize':sys.flags.optimize,'python_debug':__debug__,'no_execution_or_approval':True})
print(json.dumps({name:rec(Q/name)['sha256']for name in ['CALLER-TARGET.json','CALLER-READBACK.json','CALLER-INPUTS.json','CALLER-DELTA.patch','CALLER-PLAN.md','LAUNCH-PLAN.json','CANCELLATION-CHRONOLOGY.md']},indent=2));print('records',len(records),'unchanged source records',len(actual_source))
