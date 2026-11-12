#!/usr/bin/python3
"""Retain finite actual outcomes only after all started phases are terminal."""
import fcntl
import json
import os
from pathlib import Path
import stat
import cache_lease
from common import check_file, file_record, json_read, require, write_new

HERE=Path(__file__).resolve().parent
OWNER=HERE.parent

def main():
    require(not (OWNER/'final-v1').exists(),'refuse old final packet mutation')
    admission=json_read(HERE/'ADMISSION.json')
    check_file(admission['caller_manifest']);check_file(admission['setup'])
    binding=admission['lease']
    fd=cache_lease.open_bound(binding)
    try:
        fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        raw=os.pread(fd,4096,0);token=json.loads(raw)
        require(raw==cache_lease.token(token['plan_sha256']),'malformed final lease token')
        completion=json_read(Path(binding['state_directory'])/(token['plan_sha256']+'.json'))
        check_file(completion['result'])
        last=json_read(completion['result']['path'])
        require(last['terminal_authenticated'] or last['observer_attempted'] is False,'last phase unresolved')
        names=['dependencies','compile',*[x['name'] for x in json_read(OWNER/'SELECTORS.json')['cases']]]
        phases=[];inputs={};cpu=0;wall=0;payload=0
        def add(p):
            r=file_record(p);prior=inputs.get(r['path']);require(prior is None or prior==r,'input changed during freeze');inputs[r['path']]=r
        links={}
        for name in names:
            result_path=HERE/'controls'/name/'result.json'
            control=HERE/'controls'/name
            require(not control.exists() or result_path.is_file(),'started phase has no final result')
            if not result_path.exists():continue
            result=json_read(result_path);plan_path=HERE/'plans'/(name+'.json');plan=json_read(plan_path)
            require(file_record(plan_path)['sha256']==result['plan_sha256'],'result plan hash differs')
            require(result['terminal_authenticated'] or result['observer_attempted'] is False,'phase unresolved')
            for row in plan['inputs']:check_file(row);add(row['path'])
            for row in plan['input_symlinks']:
                p=Path(row['path']);require(p.is_symlink() and os.readlink(p)==row['target']
                    and stat.S_IMODE(p.lstat().st_mode)==row['mode'],'input alias changed')
                old=links.get(row['path']);require(old is None or old==row,'alias changed');links[row['path']]=row
            if result['observer_attempted']:
                check_file(result['observer_result']);observed=json_read(result['observer_result']['path'])
                final=observed.get('final_accounting') or observed.get('refusal_final_accounting')
                require(final and final['cgroup_empty'] and final['properties']['MainPID']==0
                    and final['properties']['ControlGroup']=='','retained terminal accounting incomplete')
                cpu+=final['cpu_usage_nsec']/1e9;wall+=observed['elapsed_seconds']
                payload+=result.get('payload_exit',{}).get('elapsed_seconds',0)
            phases.append(dict(name=name,kind=plan['kind'],plan=file_record(plan_path),result=file_record(result_path),
                accepted=result['accepted'],outcome=result.get('outcome','failed'),raw_status=result['raw_status'],
                terminal_authenticated=result['terminal_authenticated'],inputs_unchanged=result.get('inputs_unchanged',False)))
        # Only the owned evidence tree is enumerated. The mutable compiler output
        # directory is metadata; the separately retained ELF is the review input.
        for root,dirs,files in os.walk(OWNER,followlinks=False):
            root=Path(root)
            dirs[:]=[d for d in dirs if root/d not in [HERE/'target',OWNER/'final-v1']]
            for name in files:
                p=root/name
                if stat.S_ISREG(p.lstat().st_mode):add(p)
        final_dir=OWNER/'final-v1';final_dir.mkdir(mode=0o700)
        probes=[p for p in phases if p['kind']=='probe']
        counts={k:sum(p['outcome']==k for p in probes) for k in ['passed','unexecuted','failed']}
        require(sum(counts.values())==len(probes),'outcome denominator mismatch')
        results=dict(scope='Native architectural probes only',planned_cases=42,attempted_cases=len(probes),
            unattempted_cases=42-len(probes),**counts,phases=phases,total_observed_phases=len(phases),
            aggregate_cpu_seconds=cpu,summed_observer_wall_seconds=wall,summed_payload_seconds=payload,
            all_native_cases_passed=len(probes)==42 and counts['passed']==42,
            source=file_record(OWNER/'source/probe.c'),rust_test_count=0,vm_control_count=0,hermit_parity=False,
            source_predicates_unchanged=True,prior_failure_or_unexecuted_outcomes_not_relabelled=True)
        write_new(final_dir/'RESULTS.json',results)
        write_new(final_dir/'INPUTS.json',dict(records=sorted(inputs.values(),key=lambda r:r['path']),
            symlinks=sorted(links.values(),key=lambda r:r['path']),mutable_compiler_target_not_a_review_input=True,
            retained_binary_binding=str(HERE/'retained-binaries/BINDING.json')))
        report=(f"# Native timestamp probe evidence\n\n{len(probes)}/42 native cases attempted: {counts['passed']} passed, "
                f"{counts['unexecuted']} unexecuted, {counts['failed']} failed; {42-len(probes)} unattempted. "
                f"{len(phases)} observed phases. Aggregate CPU {cpu:.6f}s; summed observer wall {wall:.6f}s; "
                f"summed payload {payload:.6f}s.\n\nAll original raw results, source, plans and completed accounting remain bound. "
                "Compiler output paths are metadata; the independent retained ELF is the probe input. "
                "Exit 77 is unexecuted and never a pass. No Rust test, VM, deterministic-repeat or Hermit parity credit is supplied. "
                "RF was requested via signal return; final flags do not independently observe its input restoration.\n")
        (final_dir/'REPORT.md').write_text(report)
        for row in inputs.values():check_file(row)
        held=dict(private_lease=binding,exclusive_final_check=True,last_completion=file_record(Path(binding['state_directory'])/(token['plan_sha256']+'.json')),
                  no_shared_lane_lease=True,no_unresolved_started_phase=True)
    finally:
        os.close(fd)
    write_new(final_dir/'LEASE-RELEASE.json',{**held,'exclusive_fd_closed':True})
    write_new(final_dir/'READBACK.json',dict(inputs_unchanged=True,records=len(inputs),symlinks=len(links),
        artifacts=[file_record(final_dir/n) for n in ['RESULTS.json','INPUTS.json','REPORT.md','LEASE-RELEASE.json']],
        no_execution_by_freezer=True,original_bounds_preserved=True))
    print(str(final_dir))

if __name__=='__main__':
    main()
