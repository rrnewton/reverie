#!/usr/bin/python3
"""One authorized finite sequence. Never execute during caller preparation."""
import os
from pathlib import Path
import sys
from common import bounded_process, file_record, json_read, read, require, write_new

HERE=Path(__file__).resolve().parent
OWNER=HERE.parent

def run_helper(arguments,label,seconds):
    env={'HOME':'/home/newton','PATH':'/usr/bin:/bin','LC_ALL':'C','LANG':'C','TZ':'UTC',
         'USER':'newton','LOGNAME':'newton','XDG_RUNTIME_DIR':'/run/user/'+str(os.getuid()),
         'DBUS_SESSION_BUS_ADDRESS':'unix:path=/run/user/'+str(os.getuid())+'/bus'}
    q=bounded_process(['/usr/bin/python3','-B',*arguments],OWNER,env,HERE/'driver'/label,seconds,1024**2)
    require(q['stdout']['bytes']<=1024**2 and q['stderr']['bytes']<=1024**2,
            'helper output bound exceeded including fast exit')
    require(not q['forced'] and q.get('reap_incomplete') is not True,'helper transport did not finish')
    return q

def phase(name):
    prepared=run_helper([str(HERE/'prepare.py'),name],name+'-prepare',180)
    require(prepared['returncode']==0,'phase preparation failed; preserve raw preparation outputs')
    plan_path=HERE/'plans'/(name+'.json');record=file_record(plan_path);plan=json_read(plan_path)
    require(read(prepared['stdout']['path']).decode().strip()==str(plan_path)+' '+record['sha256'],
            'prepared plan output differs')
    invoked=run_helper([str(HERE/'native_phase.py'),'launch',str(plan_path),record['sha256']],
                       name+'-launch',plan['limits']['wall_seconds']+120)
    result_path=HERE/'controls'/name/'result.json';result=json_read(result_path)
    expected=0 if result['accepted'] else 77 if result.get('outcome')=='unexecuted' else 1
    require(invoked['returncode']==expected,'phase process status differs from retained result')
    require(result['inputs_unchanged'] and result['terminal_authenticated'],'source/terminal authentication incomplete')
    require(result['accepted'] or result.get('outcome')=='unexecuted','genuine native/compiler failure stops sequence')
    return file_record(result_path)

def main():
    require(sys.argv[1:]==['all'],'expected all; no implicit admission or selector override')
    require((HERE/'ADMISSION.json').is_file(),'root-authorized private admission required first')
    setup=json_read(HERE/'SETUP.json')
    records=[]
    records.append(phase('dependencies'))
    q=run_helper([str(HERE/'bind.py'),'headers'],'bind-headers',30)
    require(q['returncode']==0,'header binding failed')
    records.append(phase('compile'))
    q=run_helper([str(HERE/'bind.py'),'elf'],'bind-elf',30)
    require(q['returncode']==0,'ELF retention failed')
    cases=json_read(OWNER/'SELECTORS.json')['cases']
    for case in cases:
        records.append(phase(case['name']))
    results=[json_read(r['path']) for r in records[2:]]
    passed=sum(r['accepted'] is True for r in results)
    unexecuted=sum(r['outcome']=='unexecuted' for r in results)
    require(len(results)==42 and passed+unexecuted==42,'native denominator differs')
    write_new(HERE/'COMPLETED-SEQUENCE.json',dict(planned_native_cases=42,attempted_native_cases=42,
        passed=passed,unexecuted=unexecuted,failed=0,all_native_cases_passed=passed==42,
        source=setup['source'],phase_records=records,observer_phases=44,rust_tests=0,vm_controls=0,
        hermit_parity_claim=False,all_prior_failures_preserved=True,
        lease_release='Every phase closes the private lease after append-only terminal completion; no shared lane lease used.'))
    print('Native sequence complete: '+str(passed)+' passed, '+str(unexecuted)+' unexecuted; no Rust/VM/parity count.')

if __name__=='__main__':
    main()
