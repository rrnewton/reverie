from pathlib import Path
import ast,hashlib,json,runpy
area=Path.cwd()/'ignored/prejoin-failure-implementation-20260917';old=area/'qualification-execution-v4';new=area/'qualification-execution-v5';new.mkdir()
def digest(p):
 h=hashlib.sha256()
 with Path(p).open('rb') as f:
  for data in iter(lambda:f.read(1024*1024),b''):h.update(data)
 return h.hexdigest()
def bind(p):
 p=Path(p);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=digest(p))
def write(p,value):
 with p.open('x') as f:f.write(value)
plan=json.loads((old/'plan.json').read_text());original_stages=plan['stages'];summary=json.loads((old/'run-1/summary.json').read_text())
assert summary['status']=='failed' and summary['error']=='incomplete service accounting' and summary['active_stage']=='static-elf-05'
assert [r['test'] for r in summary['completed']]==[s['test'] for s in original_stages[:8]]
for key in ['run_root','observer_root','tmpdir']:plan[key]=plan[key].replace('qualification-execution-v4','qualification-execution-v5')
plan['environment_fixed']['TMPDIR']=plan['environment_fixed']['TMPDIR'].replace('qualification-execution-v4','qualification-execution-v5');plan['execution']=['/usr/bin/python3','-B',str(new/'launch.py')]
plan['stages']=json.loads(json.dumps(original_stages[8:]).replace('qualification-execution-v4','qualification-execution-v5'))
assert len(plan['stages'])==18 and plan['stages'][0]['name']=='static-elf-05' and plan['stages'][-1]['name']=='static-elf-22'
extra=[old/'plan.json',old/'launch.py',old/'RESULT.json',old/'REPORT.md',old/'run-1/summary.json',old/'run-1/launch.json',old/'run-1/static-elf-05-independent-terminal.json']
for stage in original_stages[:9]:
 for suffix in ['dispatch.json','readback.json','outcome.json','service-post.json']:
  p=old/'run-1'/(stage['name']+'-'+suffix)
  if p.exists():extra.append(p)
 extra.extend(Path(stage['out'])/name for name in ['result.json','stdout','stderr'])
 admission=Path(stage['admission_record']);extra.extend([admission,admission.with_suffix('.exit.json')])
known={r['path']:r for r in plan['inputs']}
for p in extra:
 row=bind(p)
 if row['path'] in known:assert known[row['path']]==row
 else:plan['inputs'].append(row);known[row['path']]=row
plan['continuation']={'original_plan':str(old/'plan.json'),'original_summary':str(old/'run-1/summary.json'),'original_result':str(old/'RESULT.json'),'original_report':str(old/'REPORT.md'),'accepted_prefix_count':8,'retry_stage':'static-elf-05','first_attempt_tail':['static-elf-'+str(i).zfill(2) for i in range(6,23)],'reason':'Original static-elf-05 accounting refusal is retained; this attempt repeats that one method and executes the final 17 original methods for the first time on v22.'}
plan['scope']='Original 26-method Reverie qualification continued after the retained observer accounting refusal: 8 accepted prior methods (4 VM + 4 static), original static-elf-05 retried with unchanged admission/bounds, then 17 previously unexecuted original static methods. No claim of 26 clean first attempts. Same frozen source and actual ELFs; no Hermit/Detcore or canonical parity evidence.'
for key in ['run_root','observer_root']:assert not Path(plan[key]).exists()
write(new/'plan.json',json.dumps(plan,indent=2)+'\n')
s=(old/'launch.py').read_text().replace(digest(old/'plan.json'),digest(new/'plan.json'))
old_check="""    require(len(selected) == 26 and len(set(selected)) == 26 and
            set(selected) == set(plan['selected_tests']['lib'] + plan['selected_tests']['static-elf']),
            'missing or duplicated test stage')
"""
new_check="""    continuation = plan['continuation']
    prior_plan = json.loads(read_bounded(continuation['original_plan'], 1024**2))
    prior_summary = json.loads(read_bounded(continuation['original_summary'], 1024**2))
    require(prior_plan['source_binding'] == plan['source_binding'] and
            prior_plan['source_manifest'] == plan['source_manifest'] and
            prior_plan['artifacts'] == plan['artifacts'] and
            prior_plan['selected_tests'] == plan['selected_tests'],
            'continuation changed source, executables or original population')
    require(prior_summary['status'] == 'failed' and
            prior_summary['error'] == 'incomplete service accounting' and
            prior_summary['active_stage'] == 'static-elf-05', 'wrong original refusal')
    prior_completed = prior_summary['completed']
    original_order = [step['test'] for step in prior_plan['stages']]
    require(len(original_order) == 26 and len(set(original_order)) == 26 and
            set(original_order) == set(plan['selected_tests']['lib'] + plan['selected_tests']['static-elf']),
            'original population is not the complete 26 methods')
    require(continuation['accepted_prefix_count'] == 8 and
            len(prior_completed) == 8 and
            [row['test'] for row in prior_completed] == original_order[:8] and
            all(row['status'] == 'passed' for row in prior_completed),
            'accepted original prefix is incomplete')
    require(len(selected) == 18 and selected == original_order[8:] and
            [step['name'] for step in plan['stages']] ==
            ['static-elf-' + str(index).zfill(2) for index in range(5, 23)],
            'continuation skipped, reordered or duplicated an original method')
    for stage, accepted in zip(prior_plan['stages'][:8], prior_completed):
        retained = json.loads(read_bounded(Path(continuation['original_summary']).parent /
                             (stage['name'] + '-outcome.json'), 1024**2))
        result = json.loads(read_bounded(Path(stage['out']) / 'result.json', 1024**2))
        require(retained == accepted and result['accounting_complete'] is True and
                result['comparison_eligible'] is True and result['wrapper_exit_code'] == 0 and
                result['observer_error'] is None and result['stop_reason'] is None,
                'original accepted method lacks its successful observation')
    refused = json.loads(read_bounded(Path(prior_plan['stages'][8]['out']) / 'result.json', 1024**2))
    require(refused['accounting_complete'] is False and refused['comparison_eligible'] is False and
            refused['observer_error'] == 'OSError: [Errno 19] No such device',
            'original accounting refusal was changed or relabelled')
"""
assert s.count(old_check)==1;s=s.replace(old_check,new_check)
old_launch="""              artifacts=plan['artifacts'], selected_tests=plan['selected_tests'], scope=plan['scope']))"""
new_launch="""              artifacts=plan['artifacts'], selected_tests=plan['selected_tests'],
              continuation=continuation, prior_accepted=prior_completed, scope=plan['scope']))"""
assert s.count(old_launch)==1;s=s.replace(old_launch,new_launch)
s=s.replace("error=str(error), completed=records, scope=plan['scope']))","error=str(error), completed=records, continuation=continuation, prior_accepted=prior_completed, scope=plan['scope']))")
s=s.replace("dict(status='passed', completed=records, scope=plan['scope']))","dict(status='passed', completed=records, continuation=continuation, prior_accepted=prior_completed, scope=plan['scope']))")
ast.parse(s);write(new/'launch.py',s)
functions=runpy.run_path(plan['helpers']['path'],run_name='preflight_only');functions['check_inputs'](plan)
for artifact in plan['artifacts'].values():functions['check_executable'](artifact)
record={'plan':bind(new/'plan.json'),'caller':bind(new/'launch.py'),'execution':plan['execution'],'inputs':len(plan['inputs']),'original_methods':26,'accepted_prefix':8,'retry_methods':1,'previously_unexecuted_tail':17,'scope':'Preparation only; no execution.'}
write(new/'reservation.json',json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
