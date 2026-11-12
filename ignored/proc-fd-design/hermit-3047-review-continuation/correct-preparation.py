from pathlib import Path
import datetime,difflib,hashlib,json,shutil,stat
p=Path(__file__).resolve().parent
saved=p/'preparation-v1';saved.mkdir(exist_ok=False)
for n in ['prompt.txt','EVIDENCE-APPENDIX.md','input-binding.json','PREPARATION-READBACK.json','PLAN.md']:
 shutil.copy2(p/n,saved/n)
text=(p/'prompt.txt').read_text()
a='Completed-container and ambient mount provenance must agree exactly before KVM starts; configured/orphaned/malformed state must refuse without mutation.'
b='Completed-container and ambient mount provenance must agree exactly before KVM starts. Valid absent provenance is captured. Ambient capture failure, malformed rows, mismatched captured order, or nonempty uncaptured identity vectors must refuse without mutation.'
assert text.count(a)==1;text=text.replace(a,b)
a='The official full pinned-image Rust cat method on a9 has a durable validation record and is materializing/running at this appendix cutoff; there is no test result yet. Treat it as pending, not passed or failed. A later result will require separate attribution.'
b='The official full pinned-image Rust cat attempt on a9 refused before admission, so no tests ran. The default local admission checkout lacks unpublished a9, then GitHub compare returned HTTP404; separate inspection confirmed the owned-slot floor ancestry passes and GitHub GET commit a9 returns HTTP422. This unresolved source lookup is not a failed cat assertion. The original assertion remains pending. The retained admission record/log are in appendix/official-admission; a later test result requires separate attribution.'
assert text.count(a)==1;text=text.replace(a,b);(p/'prompt.txt').write_text(text)
text=(p/'EVIDENCE-APPENDIX.md').read_text()
a='The original full pinned-image Rust cat method is pending on a9 at this cutoff. Root reports durable unit validate-kvm-closed-stdin-a9d3b1fa-official.service and record ignored/validate/runs/validate-kvm-closed-stdin-a9d3b1fa-official.json, currently materializing with no test result yet. This paragraph records pending status only; no success, failure or completed test execution is asserted. The descriptor/proc-stat controls do not replace that original obligation.'
b='The original full pinned-image Rust cat attempt on a9 refused before admission; no tests ran, and the original assertion remains pending. The retained record names validate-kvm-closed-stdin-a9d3b1fa-official.service, admission exit3/reason stale-base, and materialized_target=false; executed-test fields remain null, not an executed failure count. The retained log reports an unresolved fixed-floor lookup because GitHub compare returned HTTP404. Root independently found that the default local admission checkout lacks unpublished a9, while floor ancestry in the owned slot passes; a separate GitHub GET commit a9 returned HTTP422. This is a source-availability/admission refusal, not evidence that the cat assertion failed or passed. The exact record and333-byte log are copied under appendix/official-admission. The descriptor/proc-stat controls do not replace that original obligation.'
assert text.count(a)==1;text=text.replace(a,b);(p/'EVIDENCE-APPENDIX.md').write_text(text)
text=(p/'PLAN.md').read_text();text=text.replace('The original full pinned-image Rust cat test is pending at this package cutoff, with its durable validation unit materializing and no test result asserted.','The original full pinned-image Rust cat attempt refused before admission with no tests run; its assertion remains pending. The exact admission record/log and narrow provenance clarification are preserved in the v2 correction.')
(p/'PLAN.md').write_text(text)
def sha(b):return hashlib.sha256(b).hexdigest()
def row(q):
 b=q.read_bytes();return {'path':str(q),'bytes':len(b),'sha256':sha(b),'mode':oct(stat.S_IMODE(q.stat().st_mode))}
inputs=json.loads((saved/'input-binding.json').read_text())
for n in ['prompt.txt','EVIDENCE-APPENDIX.md']:
 idx=next(i for i,x in enumerate(inputs) if x['path']==str(p/n));inputs[idx]=row(p/n)
newdir=p/'appendix/official-admission';newdir.mkdir(exist_ok=False)
for source,name,expected in [
 ('/home/newton/work/dev-hermit/ignored/validate/runs/validate-kvm-closed-stdin-a9d3b1fa-official.json','run-record.json','1a47f8fd96895338baa4dd7c15cce0a12efa947244576830f1eb9ae64532e911'),
 ('/home/newton/work/dev-hermit/ignored/validate/validate-kvm-closed-stdin-a9d3b1fa-official.log','admission.log','94dddce718a1b8a47045ea5123663fc028d113a5404acc904a46efc6adcefc8d')]:
 q=Path(source);b=q.read_bytes();assert sha(b)==expected
 target=newdir/name;target.write_bytes(b);target.chmod(stat.S_IMODE(q.stat().st_mode));inputs.append(row(target))
(p/'input-binding.json').write_text(json.dumps(inputs,indent=2)+'\n')
diff=''
for n in ['prompt.txt','EVIDENCE-APPENDIX.md']:
 diff+=''.join(difflib.unified_diff((saved/n).read_text().splitlines(True),(p/n).read_text().splitlines(True),fromfile='preparation-v1/'+n,tofile='preparation-v2/'+n))
(p/'factual-corrections-v2.diff').write_text(diff)
inputdiff=''.join(difflib.unified_diff((saved/'input-binding.json').read_text().splitlines(True),(p/'input-binding.json').read_text().splitlines(True),fromfile='preparation-v1/input-binding.json',tofile='preparation-v2/input-binding.json'))
(p/'input-binding-v2.diff').write_text(inputdiff)
for x in inputs:assert row(Path(x['path']))==x,x['path']
assert sha((p/'launch-review.py').read_bytes())=='f0adcef76eb824ee986c6ec05d707a6fc89de50e0f0fb25eae36ab8754a0ad0a'
assert sha((p/'execution-plan.json').read_bytes())=='66df1e89808a904eaca07e959dde60ea46ec07347cd784fa1b2e14b939d4b26b'
assert not any((p/n).exists() for n in ['launch.json','stdout.jsonl','stderr.log','exit.json'])
result={'prepared_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'correction':'Only two factual topics changed in prompt/appendix; original preparation preserved. Two actual admission artifacts added.',
 'input_count':len(inputs),'input_bytes':sum(x['bytes'] for x in inputs),'unchanged_prior_input_rows':441,'changed_input_rows':2,'added_input_rows':2,
 'all_bound_files_modes_bytes_verified':True,'original_attempt_untouched':True,'caller_and_plan_unchanged':True,'no_launch':True,
 'files':{n:row(p/n) for n in ['prompt.txt','EVIDENCE-APPENDIX.md','input-binding.json','factual-corrections-v2.diff','input-binding-v2.diff','launch-review.py','execution-plan.json']}}
(p/'CORRECTIONS-v2-READBACK.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result,indent=2))
