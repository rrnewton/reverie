from pathlib import Path
import collections, hashlib, json, re, stat
E=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917/ignored/m2-controls-v1')
D=Path(__file__).parent
files={}
objects={}
def read(rel):
    path=E/rel
    assert path.is_relative_to(E) and not path.is_symlink()
    data=path.read_bytes()
    files[str(rel)]={'path':str(path),'bytes':len(data),'mode':stat.S_IMODE(path.stat().st_mode),'sha256':hashlib.sha256(data).hexdigest()}
    copy=D/'inputs'/rel
    if not copy.exists():
        copy.parent.mkdir(parents=True,exist_ok=True);copy.write_bytes(data);copy.chmod(files[str(rel)]['mode'])
    return data
def obj(rel):
    v=json.loads(read(rel));objects[str(rel)]=v;return v
def digest(rel):return hashlib.sha256(read(rel)).hexdigest()
def text(rel):return read(rel).decode()
co=obj('COHORTS.json');pre=obj('PRELIMINARY-RESULTS.json');read('PRELIMINARY-REPORT.md')
for n in ['admit.py','readback.py','run_phase.py','common.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','launch-controls-v2.sh','prepare-package.py','prepare-package-v2.py','package-v2.patch','prepare-control-v2.py','sequence-template.json']:
    read(n)
rows=[]
for ph,c in co['selected'].items():
    op=Path('observer')/ph;cp=Path('controls-run-1')/ph
    p=obj(ph+'-plan.json');ct=obj(ph+'-context.json');b=obj(cp/'before.json');a=obj(op/'admission.json');o=obj(op/'result.json');r=obj(cp/'result.json');pe=obj(op/'payload-exit.json');be=obj(op/'before-exec.json');receipt=obj(op/'before-exec-receipt.json');post=obj(cp/'service-post-readback.json')
    sb=obj(cp/'scm-before-binding.json');sa=obj(cp/'scm-after-binding.json')
    for part in ('before','after'):
        for q in ('head','tree','branch','index'):
            obj(cp/f'scm-{part}-{q}.json');read(cp/f'scm-{part}-{q}.stdout');read(cp/f'scm-{part}-{q}.stderr')
    launch=obj(op/'launch.json');read(op/'safehermit.report')
    for suffix in ('prepare.status','prepare.stdout','prepare.stderr','launch.status','launch.stdout','launch.stderr'):
        read(ph+'-'+suffix)
    stdout=text(op/'stdout');stderr=text(op/'stderr')
    samples=[json.loads(l) for l in text(op/'samples.jsonl').splitlines()]
    actual=re.findall(r'^test (.+) \.\.\. (ok|FAILED|ignored)$',stdout,re.M)
    names=[n for n,s in actual];counts=collections.Counter(s for n,s in actual)
    pr=next(row for row in pre['rows'] if row['phase']==ph)
    selected=c['names']; runtime=p['phase_bindings']['runtime_executables'][0]
    ns=o['final_accounting']['cpu_usage_nsec'];cpu=ns/1e9;wall=o['elapsed_seconds'];service=o['authenticated_service'];props=service['initial_properties'];final=o['final_accounting']['properties']
    cleanprops={'LoadState':'not-found','ActiveState':'inactive','SubState':'dead','MainPID':'0','ControlGroup':''}
    postchecks=[]
    for n,po in enumerate(post):
        raw=text(cp/f'service-post-{n}.stdout');err=text(cp/f'service-post-{n}.stderr');obj(cp/f'service-post-{n}.json')
        parsed=dict(l.split('=',1) for l in raw.splitlines())
        postchecks.append({'properties':parsed,'raw_matches_record':parsed==po['properties'],'terminal_empty':parsed==cleanprops,'returncode':po['returncode'],'forced':po['forced'],'stderr_empty':not err,'unit':po['argv'][3]})
    # Read only retained paths under the assigned evidence root.
    for q in [o['final_report']['log_file'],o['final_report']['note.run_record'].split(' (')[0]]:
        qp=Path(q)
        assert qp.is_relative_to(E)
        read(qp.relative_to(E))
    log=read(Path(o['final_report']['log_file']).relative_to(E))
    source_records=[co['source'],pre['source'],ct['source_manifest'],b['source'],r['source']]
    checks={
        'selected_equals_plan_expected':selected==p['phase']['expected_selected'],
        'selected_equals_plan_selected':selected==p['phase']['selected_names'],
        'selected_equals_binding':selected==p['phase_bindings']['selected_names'],
        'selected_equals_actual_order':selected==names,
        'all_names_unique':len(names)==len(set(names)),
        'actual_argv_exact':a['argv']==p['phase']['argv']==[runtime['path'],'--exact']+selected+['--nocapture','--test-threads=1'],
        'preliminary_names_equal':pr['passed_names']==[n for n,s in actual if s=='ok'] and pr['failed_names']==[n for n,s in actual if s=='FAILED'] and pr['ignored_names']==[n for n,s in actual if s=='ignored'],
        'no_ignored':counts['ignored']==0,
        'recorded_executables_equal':a['executable_bindings']==b['executables']==ct['executables'],
        'runtime_in_all_executable_records':all(runtime in x for x in [a['executable_bindings'],b['executables'],ct['executables']]),
        'source_records_equal':all(x==source_records[0] for x in source_records),
        'source_final_readback_recorded':r['final_source_inputs_unchanged'] is True,
        'scm_before_after_equal':sb==sa==ct['scm'],
        'plan_sha_matches_admission_before_exec':digest(ph+'-plan.json')==a['phase_plan_sha256']==be['payload']['arguments'][-1],
        'before_exec_payload_authorization_matches':o['payload_authorization']['request']['payload']==o['payload_authorization']['response']['payload']==be['payload'],
        'before_exec_spec_sha_matches':digest(op/'before-exec.json')==o['payload_authorization']['request']['spec_sha256']==o['payload_authorization']['response']['spec_sha256'],
        'requested_environment_equal':a['environment']==p['environment'] and all(a['actual_environment'].get(k)==v for k,v in p['environment'].items()),
        'payload_exit_matches_controller':pe==r['payload_exit'],
        'exit_statuses_match':pe['returncode']==r['raw_status']==o['wrapper_exit_code']==o['final_accounting']['exec_main_status']==final['ExecMainStatus']==int(o['final_report']['exit_code'])==pr['raw_status'],
        'payload_reaped_without_timeout':pe['reaped'] and not pe['local_wait_timed_out'],
        'no_forced_observer_stop':o['stop_reason'] is None and o['observer_error'] is None and not r['transport']['forced'],
        'acceptance_preserves_failure':r['accepted']==o['comparison_eligible']==pr['accepted']==(counts['FAILED']==0 and pe['returncode']==0),
        'terminal_authenticated':r['terminal_authenticated'] and o['accounting_complete'] and o['final_accounting']['cgroup_empty'],
        'terminal_service_identity':final['Id']==service['unit']==props['Id'] and final['ExecMainPID']==props['ExecMainPID']==service['main_process']['pid'] and final['ExecMainStartTimestampMonotonic']==props['ExecMainStartTimestampMonotonic'],
        'terminal_properties_empty':final['ActiveState']=='inactive' and final['SubState']=='dead' and final['MainPID']==0 and final['ControlGroup']=='',
        'cpu_matches_preliminary':cpu==pr['cpu_seconds'],
        'wall_matches_preliminary':wall==pr['wall_seconds'],
        'sample_count_matches':len(samples)==o['sample_count'],
        'sample_max_matches':max(s['usage_usec'] for s in samples)==o['maximum_sampled_usage_usec'],
        'final_cpu_covers_sample_max':ns>=o['maximum_sampled_usage_usec']*1000,
        'bounds_preserved':p['phase']['aggregate_cpu_usec']==30000000 and p['phase']['wall_seconds']==60 and p['phase']['memory_bytes']==17179869184 and p['phase']['swap_bytes']==0 and p['phase']['lethal_stderr_bytes']==1048576 and props['RuntimeMaxUSec']==60000000 and props['MemoryMax']==17179869184 and props['MemorySwapMax']==0,
        'below_cpu_wall_stderr_bounds':cpu<30 and wall<60 and len(stderr.encode())<1048576,
        'stderr_equals_safehermit_log':stderr.encode()==log and len(log)==int(o['final_report']['bytes_written']) and o['final_report']['truncated']=='false',
        'all_service_postchecks_clean':all(z['raw_matches_record'] and z['terminal_empty'] and z['returncode']==0 and not z['forced'] and z['stderr_empty'] and z['unit']==service['unit'] for z in postchecks),
    }
    assert counts['ok']+counts['FAILED']+counts['ignored']==len(selected)
    rows.append({'phase':ph,'checks':checks,'selected_names':selected,'actual_names_and_outcomes':[{'name':n,'outcome':s} for n,s in actual],'counts':dict(counts),'summary':re.findall(r'^test result:.*$',stdout,re.M),'payload_exit':pe,'raw_status':r['raw_status'],'accepted':r['accepted'],'terminal_authenticated':r['terminal_authenticated'],'comparison_eligible':o['comparison_eligible'],'controller_error':r.get('error'),'cpu_seconds':cpu,'observed_wall_seconds':wall,'transport_wall_seconds':r['transport']['elapsed_seconds'],'sample_count':len(samples),'runtime_executable_record':runtime,'source_record':r['source'],'scm_record':sb,'service':service['unit'],'service_postchecks':postchecks,'requested_environment_keys':sorted(a['environment']),'additional_actual_environment_keys':sorted(set(a['actual_environment'])-set(a['environment'])),'cwd':p['phase']['cwd'],'plan_admission':p['admission'],'panics':[{'line':n+1,'text':'\n'.join(stderr.splitlines()[n:n+5])} for n,l in enumerate(stderr.splitlines()) if 'panicked at' in l]})
prep={}
for prefix in ['prepare-package','prepare-package-v2','sequence-v2']:
    prep[prefix]={s:text(prefix+'.'+s) for s in ('status','stdout','stderr')}
# Check every referenced record within this evidence root, retaining externally named
# source/build records as recorded identities only. Do not read those external paths.
links=[]
def walk(value,origin):
    if isinstance(value,dict):
        if {'path','sha256'}.issubset(value):
            p=Path(value['path'])
            if p.is_absolute() and p.is_relative_to(E):
                rel=p.relative_to(E)
                if p.exists() and p.is_file() and not p.is_symlink():
                    read(rel);actual=files[str(rel)]
                    checks={k:actual[k]==value[k] for k in ('sha256','bytes','mode') if k in value}
                    links.append({'origin':origin,'target':str(rel),'checks':checks})
                else:links.append({'origin':origin,'target':str(rel),'missing':True})
        for v in value.values():walk(v,origin)
    elif isinstance(value,list):
        for v in value:walk(v,origin)
for rel,o in list(objects.items()):walk(o,rel)
link_failures=[x for x in links if x.get('missing') or not all(x['checks'].values())]
result={'scope':'Retained native cohort records only; no live source or cache hash and no product execution. Compile and six ELF inventories are parent-owned.','rows':rows,'preparation_records':prep,'absent_unit_modules':co['absent_unit_modules'],'retained_link_checks':links,'retained_link_failures':link_failures,'failed_audit_checks':[{'phase':r['phase'],'check':k} for r in rows for k,v in r['checks'].items() if not v]}
(D/'AUDIT.json').write_text(json.dumps(result,indent=2)+'\n')
(D/'INPUTS.json').write_text(json.dumps(list(files.values()),indent=2)+'\n')
print(json.dumps({'audit_check_failures':result['failed_audit_checks'],'retained_link_failures':link_failures,'files':len(files),'link_checks':len(links),'rows':[{k:r[k] for k in ['phase','counts','raw_status','accepted','cpu_seconds','observed_wall_seconds','runtime_executable_record']} for r in rows]},indent=2))
