from pathlib import Path
import hashlib,json,os,stat,subprocess,datetime
p=Path(__file__).resolve().parent
old=p.parent/'hermit-3047-review-preparation'
h=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parity-recovery-20260916')
r=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917')
e=h/'ignored/recovery/mount-integration-596b9ade'
l=Path('/home/newton/work/dev-hermit/ignored/kvm-parity-20260914-codex/next-work/queue-drain-20260916/parent-ledger')
head='a9d3b1faa4502963e33bcb3e7e8a213a4e1a3bb0';base='b03fd6c16a438060f0013d58948643d8f3d81ab5';prior='aa7ea4827b8328345e715d76518d7f2205e41110';priorbase='35af18f34a3238117eecac8b439ac20e13316715'
records=[]
def sha(b):return hashlib.sha256(b).hexdigest()
def git(repo,*args):return subprocess.check_output(['git',*args],cwd=repo,env={**os.environ,'GIT_NO_LAZY_FETCH':'1','GIT_OPTIONAL_LOCKS':'0'})
def put(name,b,mode=0o644,origin=None):
 q=p/name;q.parent.mkdir(parents=True,exist_ok=True)
 with q.open('xb') as f:f.write(b)
 q.chmod(mode);row={'path':str(q),'bytes':len(b),'sha256':sha(b),'mode':oct(mode)}
 if origin:row['origin']=origin
 records.append(row);return row

def copy(src,name,expected=None):
 b=src.read_bytes()
 if expected:assert sha(b)==expected,(str(src),sha(b),expected)
 return put(name,b,stat.S_IMODE(src.stat().st_mode),{'path':str(src),'sha256':sha(b)})
def tree(rev):
 result={}
 for row in git(h,'ls-tree','-r','-z',rev).split(b'\0'):
  if row:
   meta,name=row.split(b'\t');result[name.decode()]=meta.decode().split()
 return result
old_inputs=json.loads((old/'input-binding.json').read_text())
for x in old_inputs:
 q=Path(x['path']);b=q.read_bytes();assert len(b)==x['bytes'] and sha(b)==x['sha256'] and oct(stat.S_IMODE(q.stat().st_mode))==x['mode']
# Carry only public text and successful permitted tool requests/results. Hidden
# thinking, signatures, system/session metadata and other blocks are omitted.
raw=(old/'stdout.jsonl').read_bytes();assert sha(raw)=='df3548c9ed1c9dc678ceecef79de5a4c1baba9e5e3761b6c25078169db797518'
wire=[json.loads(line) for line in raw.decode().splitlines() if line]
requests={};results={};public_text=[];excluded=0
for event in wire:
 for b in event.get('message',{}).get('content',[]):
  typ=b.get('type')
  if typ=='tool_use':
   assert b['name'] in ['Read','Grep','Glob'];requests[b['id']]={'id':b['id'],'name':b['name'],'input':b['input']}
  elif typ=='tool_result':results[b['tool_use_id']]={'tool_use_id':b['tool_use_id'],'content':b['content'],'is_error':bool(b.get('is_error',False))}
  elif typ=='text' and event.get('type')=='assistant':public_text.append(b['text'])
  elif typ in ['thinking','redacted_thinking']:excluded+=1
assert len(requests)==len(results)==76
success={i for i,result in results.items() if not result['is_error']}
transcript=[];index=['The prior attempt returned no final verdict. This index carries public requests/results only; hidden thinking is excluded.\n']
for event in wire:
 for b in event.get('message',{}).get('content',[]):
  typ=b.get('type')
  if typ=='text' and event.get('type')=='assistant':transcript.append({'kind':'assistant_public_text','text':b['text']})
  elif typ=='tool_use' and b['id'] in success:transcript.append({'kind':'tool_request',**requests[b['id']]})
  elif typ=='tool_result' and b['tool_use_id'] in success:transcript.append({'kind':'tool_result',**results[b['tool_use_id']]})
for n,(ident,request) in enumerate(requests.items(),1):
 if ident not in success:continue
 result=results[ident];content=result['content']
 rendered=content if isinstance(content,str) else json.dumps(content,ensure_ascii=False,indent=2)
 name=f'prior-public/{n:03d}-{request["name"]}.txt'
 body='Prior public request:\n'+json.dumps(request,ensure_ascii=False,indent=2)+'\n\nSuccessful public tool result (exact text below):\n'+rendered
 put(name,body.encode())
 subject=request['input'].get('file_path') or request['input'].get('path') or ''
 index.append(f'{n}. {request["name"]}: {subject}; input {json.dumps(request["input"],ensure_ascii=False)}; retained result {name}\n')
put('prior-public-transcript.json',json.dumps(transcript,ensure_ascii=False,indent=2).encode()+b'\n')
put('PRIOR-READ-INDEX.md','\n'.join(index).encode())
transcript_binding={'original_stream_sha256':sha(raw),'public_text_blocks':len(public_text),'successful_tool_pairs':len(success),'failed_tool_pairs_omitted':len(requests)-len(success),'hidden_blocks_excluded':excluded,'terminal_result_count':sum(x.get('type')=='result' for x in wire),'original_actual_exit':124,'original_elapsed_seconds':900.0190454360563,'schema':'Only whitelisted public text/tool requests/tool results; no thinking/signature/system/session fields copied.'}
put('public-transcript-binding.json',json.dumps(transcript_binding,indent=2).encode()+b'\n')
copy(old/'prompt.txt','original-mandate.txt','a00124533667ccda0698c7ab0ef1140461d805c66b0d159143b8cd9fd9707ea0')
copy(old/'RESULTS.md','prior-attempt-RESULTS.md')
copy(old/'exit.json','prior-attempt-exit.json','599e33908adf6f0e218e6ae79defd72c5dea9066d4d0869f76f11aff7745c090')
copy(old/'COMPLETION-READBACK.json','prior-attempt-COMPLETION-READBACK.json')
patch=git(h,'diff','--binary',base,head)
assert sha(patch)=='96502f9880df67b8423b1ed111c65f1f08feb0eede6c30657db25f281f92083a'
copy(old/'complete-hermit.patch','complete-hermit.patch',sha(patch))
put('incoming-main.patch',git(h,'diff','--binary',priorbase,base))
put('actual-aa7-to-a9.patch',git(h,'diff','--binary',prior,head))
put('actual-commits.txt',git(h,'log','--reverse','--format=fuller',base+'..'+head))
t0,t1,tm,ta=tree(priorbase),tree(prior),tree(base),tree(head)
changed={x for x in set(t0)|set(t1) if t0.get(x)!=t1.get(x)}
incoming={x for x in set(t0)|set(tm) if t0.get(x)!=tm.get(x)}
assert len(changed)==21 and len(incoming)==8 and not changed&incoming
expected={x:(t1.get(x) if x in changed else tm.get(x)) for x in set(t1)|set(tm)}
expected={x:v for x,v in expected.items() if v is not None}
assert ta==expected and len(ta)==1726
commits=[]
for a,b in [('df8f668f9956797ffaeb72a26a938619c0be5df9','1d695a78051b84febaec32ee358a1d45296f2405'),(prior,head)]:
 x=git(h,'cat-file','commit',a);y=git(h,'cat-file','commit',b)
 xh,xm=x.split(b'\n\n',1);yh,ym=y.split(b'\n\n',1)
 assert xm==ym
 assert next(z for z in xh.splitlines() if z.startswith(b'author '))==next(z for z in yh.splitlines() if z.startswith(b'author '))
 commits.append({'old':a,'new':b,'author_equal':True,'full_message_equal':True,'message_sha256':sha(xm)})
source=[]
old_source=json.loads((old/'candidate-binding.json').read_text())
for row in old_source['files']:
 o=row['origin'];repo=Path(o['repository']);rev=head if repo==h else o['revision'];name=o['path']
 entry=git(repo,'ls-tree',rev,'--',name).decode().strip();meta,actual_name=entry.split('\t');mode,typ,blob=meta.split()
 assert actual_name==name and typ=='blob' and mode in ['100644','100755']
 b=git(repo,'cat-file','blob',blob);assert sha(b)==row['sha256']
 dst='source/'+row['path'];srcrow=put(dst,b,int(mode[-3:],8),{'repository':str(repo),'revision':rev,'path':name,'git_blob':blob,'git_mode':mode})
 source.append(srcrow)
for name in sorted(incoming|{'detcore/src/consts.rs'}):
 mode,typ,blob=git(h,'ls-tree',head,'--',name).decode().strip().split('\t')[0].split();assert typ=='blob'
 source.append(put('source/hermit/'+name,git(h,'cat-file','blob',blob),int(mode[-3:],8),{'repository':str(h),'revision':head,'path':name,'git_blob':blob,'git_mode':mode}))
composition={'base':base,'head':head,'tree':git(h,'rev-parse',head+'^{tree}').decode().strip(),'prior_base':priorbase,'prior_head':prior,'all1726entries_equal_exact_union':True,'changed_paths':sorted(changed),'incoming_main_paths':sorted(incoming),'overlap_paths':[],'authored_patch_identical':True,'patch_sha256':sha(patch),'authors_and_messages':commits,'no_runtime_result_transferred_to_new_head':True}
put('composition-readback.json',json.dumps(composition,indent=2).encode()+b'\n')
copy(e/'CHECKS-COMPLETED.json','appendix/CHECKS-COMPLETED.json','62751fa45f88b1f1720c11a137255594775841219199fdecb473062b47148934')
checks=json.loads((e/'CHECKS-COMPLETED.json').read_text())
for c in checks['commands']:
 stem=c['name'];assert c['exit_code']==0 and c['source_unchanged'] and c['scope_inactive_empty']
 for suffix in ['.json','-readback.json','.log','-stderr.log']:
  expected_digest=c['receipt_sha256'] if suffix=='.json' else c['readback_sha256'] if suffix=='-readback.json' else None
  copy(e/(stem+suffix),'appendix/checks/'+stem+suffix,expected_digest)
copy(e/'mount-17-inventory-comparison.json','appendix/checks/mount-17-inventory-comparison.json',checks['inventory_comparison_sha256'])
copy(e/'main-b03-composition/FINAL-READBACK.json','appendix/composition/FINAL-READBACK.json','8b350feba28660a91ef900e4b96e389a232cf4e0add1c62e4ed06f52e80cc2c8')
for name in ['REVIEW.md','READBACK.json']:
 copy(l/'main-b03-composition-review-a9d3b1fa'/name,'appendix/composition/independent-'+name)
for name in ['FINAL-READBACK.json','RESULT.md','plan.json']:
 copy(e/'descriptor-reuse-aa7ea482'/name,'appendix/descriptor-reuse/'+name)
descriptor=json.loads((e/'descriptor-reuse-aa7ea482/FINAL-READBACK.json').read_text())
for d in descriptor['results']:
 assert d['guest_exit']==0 and all(d['checks'].values())
 copy(Path(d['result_path']),'appendix/descriptor-reuse/raw/'+d['name']+'/result.json',d['result_sha256'])
 for key in ['stdout','stderr']:
  x=d[key];copy(Path(x['path']),'appendix/descriptor-reuse/raw/'+d['name']+'/'+key,x['sha256'])
complete_dir=l/'complete-stat-routes-aa7ea482-preparation'
for name in ['FINAL-READBACK.json','RESULT.md']:
 copy(complete_dir/name,'appendix/complete-stat-routes/'+name)
complete=json.loads((complete_dir/'FINAL-READBACK.json').read_text());assert complete['all_three_passed'] and complete['no_retries'] and complete['all_services_inactive_empty']
for c in complete['records']:
 assert c['guest_exit']==0 and c['actual_guest_pass']
 for key in ['stdout','stderr']:
  x=c[key];q=Path(x['path']);copy(q,'appendix/complete-stat-routes/raw/'+c['step']+'/'+key,x['sha256'])
 q=Path(c['stdout']['path']).parent/'result.json';copy(q,'appendix/complete-stat-routes/raw/'+c['step']+'/result.json')
manifest={'prepared_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'files':records,'original169_inputs':old_inputs,'source_files':source,'composition':composition,'public_transcript':transcript_binding,'runtime_attribution':'All added checks and runtime probes measured aa7/a31abf55. The a9 rebase is source composition only. Full pinned-image cat remains pending at cutoff.'}
(p/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(json.dumps({'files':len(records),'bytes':sum(x['bytes'] for x in records),'public_tool_pairs':len(success),'excluded_hidden_blocks':excluded,'source_files':len(source),'head':head,'tree':composition['tree'],'manifest_sha256':sha((p/'manifest.json').read_bytes())},indent=2))
