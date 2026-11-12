from pathlib import Path
import copy,difflib,hashlib,json
A=Path(__file__).resolve().parent.parent;oldroot=A/'qualification-execution-v7';dest=A/'qualification-execution-v8';dest.mkdir();old=json.loads((oldroot/'plan.json').read_bytes());p=copy.deepcopy(old);sha=lambda p:hashlib.sha256(Path(p).read_bytes()).hexdigest()
def entry(p):
 p=Path(p);s=p.stat();return dict(path=str(p),resolved_path=str(p.resolve(strict=True)),bytes=s.st_size,mode=s.st_mode&0o7777,sha256=sha(p))
def write(p,v):
 with p.open('x') as f:json.dump(v,f,indent=2);f.write('\n')
oldout=p['observer_root'];newout=oldout.replace('qualification-execution-v7','qualification-execution-v8')
def replace(v):
 if isinstance(v,str):return v.replace(str(oldroot/'run-1'),str(dest/'run-1')).replace(oldout,newout)
 if isinstance(v,list):return [replace(x) for x in v]
 if isinstance(v,dict):return {k:replace(x) for k,x in v.items()}
 return v
p=replace(p);p['stages']=p['stages'][6:];assert len(p['stages'])==16;p['execution']=['/usr/bin/python3','-B',str(dest/'launch.py')]
p['continuation']={'original_plan':str(oldroot/'plan.json'),'original_summary':str(oldroot/'run-1/summary.json'),'accepted_prefix_count':6,'retry_stage':'static-elf-07','retry_test':p['stages'][0]['test'],'remaining_first_attempts':15,'preserve_refusal':True}
p['scope']='Authorized same-source continuation after observer accounting refusal: retry original static-elf-07 exactly once, then original 08–22 first attempts only if accepted. Preserve six accepted first attempts and the original stage 07 refusal. Same final committed 12d4ce8c source, actual ELF, full-library prerequisite, helpers, environment, original order/assertions and resource limits. Stop at first unexpected outcome; no observer change or automatic retry.'
inputs={x['path']:x for x in old['inputs']};extras=[oldroot/'plan.json',oldroot/'launch.py',oldroot/'RESULT.json',oldroot/'REPORT.md',oldroot/'TERMINAL.json',oldroot/'REFUSAL-RECOVERED-SERVICE.json']
extras += [x for x in (oldroot/'run-1').iterdir() if x.is_file()]
extras += [x for x in (oldroot/'run-1/admissions').iterdir() if x.is_file()]
for s in old['stages'][:7]:extras += [Path(s['out'])/x for x in ['stdout','stderr','result.json']]
for x in extras:inputs[str(x)]=entry(x)
p['inputs']=list(inputs.values());write(dest/'plan.json',p)
text=(oldroot/'launch.py').read_text().replace(sha(oldroot/'plan.json'),sha(dest/'plan.json'))
oldcheck="    require(len(selected) == 22 and len(set(selected)) == 22 and\n            selected == plan['selected_tests']['static-elf'],\n            'missing or duplicated test stage')\n"
prev=(A/'qualification-execution-v5/launch.py').read_text();begin=prev.index("    continuation = plan['continuation']");end=prev.index("    for kind, entry in plan['inventories'].items():",begin);block=prev[begin:end]
block=block.replace("'static-elf-05'","'static-elf-07'").replace('len(original_order) == 26 and len(set(original_order)) == 26','len(original_order) == 22 and len(set(original_order)) == 22').replace("set(plan['selected_tests']['lib'] + plan['selected_tests']['static-elf'])","set(plan['selected_tests']['static-elf'])").replace('complete 26 methods','complete 22 methods').replace("accepted_prefix_count'] == 8","accepted_prefix_count'] == 6").replace('len(prior_completed) == 8','len(prior_completed) == 6').replace('original_order[:8]','original_order[:6]').replace('len(selected) == 18','len(selected) == 16').replace('original_order[8:]','original_order[6:]').replace('range(5, 23)','range(7, 23)').replace("prior_plan['stages'][:8]","prior_plan['stages'][:6]").replace("prior_plan['stages'][8]","prior_plan['stages'][6]")
assert oldcheck in text;text=text.replace(oldcheck,block)
text=text.replace("artifacts=plan['artifacts'], selected_tests=plan['selected_tests'], scope=plan['scope']))","artifacts=plan['artifacts'], selected_tests=plan['selected_tests'],\n              continuation=continuation, prior_accepted=prior_completed, scope=plan['scope']))")
text=text.replace("error=str(error), completed=records, scope=plan['scope']))","error=str(error), completed=records, continuation=continuation, prior_accepted=prior_completed, scope=plan['scope']))")
text=text.replace("dict(status='passed', completed=records, scope=plan['scope']))","dict(status='passed', completed=records, continuation=continuation, prior_accepted=prior_completed, scope=plan['scope']))")
with (dest/'launch.py').open('x') as f:f.write(text)
with (dest/'caller-increment.patch').open('x') as f:f.writelines(difflib.unified_diff((oldroot/'launch.py').read_text().splitlines(True),text.splitlines(True),fromfile='execution-v7/launch.py',tofile='execution-v8/launch.py'))
print(json.dumps({'plan':sha(dest/'plan.json'),'caller':sha(dest/'launch.py'),'inputs':len(p['inputs'])}))
