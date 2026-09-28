"""Read-only authentication/projection of the completed, partly refused Q2 run."""
from pathlib import Path
import hashlib
import json
import os
import stat
import sys

F=Path(__file__).resolve().parent
W=F.parent.parent
Q=W/'qualification-v2'
FINAL=Q/'final-v1'

def req(value,message):
    if not value:raise RuntimeError(message)

def rec(path):
    p=Path(path);req(p.is_file() and not p.is_symlink(),str(p));h=hashlib.sha256()
    with p.open('rb') as stream:
        for b in iter(lambda:stream.read(1024*1024),b''):h.update(b)
    return dict(path=str(p),bytes=p.stat().st_size,sha256=h.hexdigest(),mode=stat.S_IMODE(p.stat().st_mode))

def check(row):
    result=rec(row['path']);req(all(result[k]==row[k] for k in result),str(row['path']));return result

def put(name,value):
    with (F/name).open('x') as stream:
        stream.write(value if isinstance(value,str) else json.dumps(value,indent=2)+'\n')

req(sys.flags.optimize==0,'optimization must be disabled')
req(rec(FINAL/'TARGET.json')['sha256']=='0b4ab5639249a24e72c1249d255217552ae0147f1287b2036fb2d24f4de9d624','final target drift')
req(rec(FINAL/'REPORT.md')['sha256']=='ab1bd984be30c698b4cffc6d1c616605fd6e0df99ce721594a27485d383ab54e','final report drift')
x=json.loads((FINAL/'RESULTS.json').read_text())
req((x['attempted_phases'],x['accepted_phases'],x['failed_or_refused_phases'],x['passed_declarations'],x['failed_or_refused_declarations'])==(45,44,1,37,1),'population drift')
require_source=Q.parent/'v2-fix/source'
expected=json.loads((Q/'PHASES.json').read_text())['phases']
req(len(expected)==45 and len(x['phases'])==45,'phase population')
held={}
def bind(row):
    actual=check(row);held[actual['path']]=actual

summary=[];raw_test=[];literal_tests=0
for phase,spec in zip(x['phases'],expected):
    req(phase['name']==spec['name'] and phase['kind']==spec['kind'] and phase['selected']==spec['selected'],'phase ordering/selection drift')
    for key in ['plan','receipt','observer','stdout','stderr','completion']:bind(phase[key])
    plan=json.loads(Path(phase['plan']['path']).read_text())
    receipt=json.loads(Path(phase['receipt']['path']).read_text())
    req(plan['limits']==spec['limits'],'phase limits drift')
    req(receipt['accepted']==phase['accepted'],'receipt admission differs')
    req(phase['terminal_authenticated'] and phase['inputs_unchanged'] and phase['accounting_complete'],'terminal/accounting incomplete')
    props=phase['final_accounting']['properties']
    req(props['ActiveState']=='inactive' and props['SubState']=='dead' and props['MainPID']==0 and phase['final_accounting']['cgroup_empty'],'not terminal')
    req(phase['raw_status']==0,'unexpected payload status')
    summary.append({k:phase[k] for k in ['name','kind','selected','raw_status','accepted','terminal_authenticated','inputs_unchanged','accounting_complete','observer_elapsed_seconds','payload_elapsed_seconds','error','readback_error']})
    if phase['kind']=='test':
        raw_test.append(f'### {phase["name"]}: {phase["selected"]}\nOriginal receipt accepted={phase["accepted"]}; raw_status={phase["raw_status"]}; readback_error={phase["readback_error"]}\n')
        for stream in ['stdout','stderr']:
            row=phase[stream];raw_test.append(f'{stream}: {row["path"]}\nSHA256 {row["sha256"]}; {row["bytes"]} bytes\n--- literal {stream} begins ---\n')
            raw_test.append(Path(row['path']).read_text());raw_test.append(f'\n--- literal {stream} ends ---\n')
        literal_tests+=1
req(literal_tests==38,'raw test population')
req([p['name'] for p in x['phases'] if not p['accepted']]==['test-38'],'refused phase changed')
for path in sorted(FINAL.iterdir()):
    if path.is_file():held[str(path)]=rec(path)
for key in ['retained_artifacts','retained_binding','prior_compiler_failure_preserved','legacy_output_inspection']:
    if {'path','bytes','sha256','mode'}<=x[key].keys():bind(x[key])
artifacts=json.loads(Path(x['retained_artifacts']['path']).read_text())
for name,artifact in artifacts.items():
    req(artifact['cargo_artifact']['fresh'] is False,'reused Cargo harness')
    req(Path(artifact['cargo_artifact']['manifest_path'])==require_source/'reverie-ptrace/Cargo.toml','wrong source attribution')
    bind(artifact['file'])
binding=json.loads(Path(x['retained_binding']['path']).read_text())
for row in binding['harnesses'].values():bind(row['retained']);req(row['distinct_inode'],'retained original inode')
for row in binding['retained_libraries']:bind(row['retained']);req(row['distinct_inode'],'retained original library inode')

# All immutable qualification input records are reauthenticated, without opening
# the excluded cache, mutable lease token, or completed control sockets.
all_inputs=json.loads((FINAL/'INPUTS.json').read_text())
for row in all_inputs['files']:check(row)
for row in all_inputs['symlinks']:
    path=Path(row['path']);req(path.is_symlink() and os.readlink(path)==row['target'],'alias drift')
put('Q2-RAW-SELECTED-OUTCOMES.txt',''.join(raw_test))
put('Q2-OUTCOME-SUMMARY.json',dict(scope='Read-only evidence projection; no change to any accepted/refused receipt',target=rec(FINAL/'TARGET.json'),results=rec(FINAL/'RESULTS.json'),attempted_phases=45,accepted_phases=44,qualified_declarations=37,legacy='test-38: raw zero and test/suite success; original mixed-stdout JSON reader refusal remains accepted=false',phases=summary,raw_selected=rec(F/'Q2-RAW-SELECTED-OUTCOMES.txt'),retained_artifacts=artifacts,python_optimize=sys.flags.optimize))
put('Q2-EVIDENCE-INPUTS.json',dict(files=sorted(held.values(),key=lambda r:r['path']),full_original_input_index=rec(FINAL/'INPUTS.json'),full_original_input_files_reauthenticated=len(all_inputs['files']),aliases_reauthenticated=len(all_inputs['symlinks']),completed_sockets_not_opened=len(all_inputs['completed_socket_metadata']),mutable_cache_and_lease_excluded=True))
put('Q2-EVIDENCE-READBACK.json',dict(summary=rec(F/'Q2-OUTCOME-SUMMARY.json'),inputs=rec(F/'Q2-EVIDENCE-INPUTS.json'),source=rec(W/'v2-fix/TARGET.json'),committed_proof=rec(F/'COMMITTED-SOURCE-PROOF.json'),full_inputs_unchanged=True,all_phases_terminal=True,all_phases_accepted=False,no_new_execution=True,python_optimize=sys.flags.optimize))
print(json.dumps(rec(F/'Q2-EVIDENCE-READBACK.json'),indent=2))
