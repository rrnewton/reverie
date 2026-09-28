from pathlib import Path
import hashlib,json,stat,subprocess,os
p=Path(__file__).resolve().parent
if not (p/'exit.json').is_file(): raise SystemExit('review has not completed; no final report written')
def sha(data):return hashlib.sha256(data).hexdigest()
def file_row(q):
 b=q.read_bytes();return {'path':str(q),'bytes':len(b),'sha256':sha(b),'mode':oct(stat.S_IMODE(q.stat().st_mode))}
inputs=json.loads((p/'input-binding.json').read_text())
verified=[]
for expected in inputs:
 actual=file_row(Path(expected['path']))
 if actual!=expected: raise RuntimeError('review input drift: '+expected['path'])
 verified.append(actual)
manifest=json.loads((p/'snapshot-manifest.json').read_text())
source_checks=[]
for r in manifest['files']:
 o=r.get('origin',{})
 if 'git_blob' not in o:continue
 data=subprocess.check_output(['git','cat-file','blob',o['git_blob']],cwd=o['repository'],env={**os.environ,'GIT_NO_LAZY_FETCH':'1','GIT_OPTIONAL_LOCKS':'0'})
 if data!=Path(r['path']).read_bytes():raise RuntimeError('immutable Git object mismatch: '+r['path'])
 source_checks.append({'path':r['path'],'git_blob':o['git_blob'],'revision':o['revision'],'sha256':sha(data)})
raw=(p/'stdout.jsonl').read_bytes()
rows=[json.loads(line) for line in raw.decode().splitlines() if line]
exit_record=json.loads((p/'exit.json').read_text())
launch=json.loads((p/'launch.json').read_text())
assert launch['head']==exit_record['head']=='aa7ea4827b8328345e715d76518d7f2205e41110'
tools=[];assistant=[]
for r in rows:
 if r.get('type')=='assistant':
  for block in r.get('message',{}).get('content',[]):
   if block.get('type')=='tool_use':tools.append({'name':block['name'],'input':block.get('input'), 'id':block.get('id')})
   if block.get('type')=='text':assistant.append(block['text'])
for t in tools:
 if t['name'] not in ['Read','Grep','Glob']:raise RuntimeError('unexpected review tool: '+t['name'])
terminal=[r for r in rows if r.get('type')=='result']
normal=exit_record['exit_code']==0 and len(terminal)==1 and not terminal[0].get('is_error',False)
report=None;literal=None
if normal:
 literal=terminal[0].get('result')
 if not isinstance(literal,str) or not literal.strip():raise RuntimeError('normal result lacks complete literal text')
 report=p/'REVIEW.md'
else:
 literal=assistant[-1] if assistant else ''
 report=p/'REVIEW-PARTIAL.md'
with report.open('xb') as f:f.write(literal.encode())
readback={'head':launch['head'],'base':launch['base'],'tree':launch['tree'],
 'actual_exit':exit_record['exit_code'],'elapsed_seconds':exit_record['elapsed_seconds'],
 'started_at':exit_record['started_at'],'finished_at':exit_record['finished_at'],
 'normal_terminal_result':normal,'result_rows':len(terminal),
 'result_is_error':terminal[0].get('is_error') if len(terminal)==1 else None,
 'source_unchanged_by_launcher':exit_record['source_unchanged'],
 'all169_inputs_modes_bytes_unchanged':len(verified)==169,'verified_input_count':len(verified),
 'git_object_copy_readbacks':source_checks,
 'requested_tools_only':sorted(set(t['name'] for t in tools)),'tool_calls':tools,
 'literal_report_extraction':'Exact JSON-decoded result.result UTF-8, without an added newline' if normal else 'Last assistant text is partial; no final approval exists',
 'final_assistant_equals_terminal_result':bool(assistant) and assistant[-1]==literal if normal else None,
 'report':file_row(report),'files':[file_row(p/n) for n in ['stdout.jsonl','stderr.log','launch.json','exit.json','input-binding.json','candidate-binding.json','prompt.txt','launch-review.py']],
 'no_review_rerun':True}
with (p/'COMPLETION-READBACK.json').open('x') as f:json.dump(readback,f,indent=2)
print(json.dumps({k:v for k,v in readback.items() if k not in ['git_object_copy_readbacks','tool_calls','files']},indent=2))
