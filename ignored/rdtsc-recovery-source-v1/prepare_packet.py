from pathlib import Path
import json,hashlib,os,difflib,re,stat
P=Path(__file__).resolve().parent;S=P/'source';B=P.parent/'publication-fd-composition-v3/source'; E=P/'evidence'
sha=lambda b:hashlib.sha256(b).hexdigest()
def write(name,obj):(P/name).write_text(json.dumps(obj,indent=2)+'\n')
def rec(p):
 st=p.lstat();o={'path':str(p),'mode':stat.S_IMODE(st.st_mode)}
 if p.is_symlink():o.update(kind='symlink',target=os.readlink(p),sha256=sha(os.fsencode(os.readlink(p))))
 else:b=p.read_bytes();o.update(kind='file',bytes=len(b),sha256=sha(b))
 return o
orig=json.loads((P/'BASELINE-MANIFEST.json').read_text());manifest=[];changes=[];blobs=[]
for item in orig:
 r=item['relative'];q=S/r;old=B/r;kind=item['kind'];m={'relative':r,'path':str(q),'mode':item['mode'],'kind':kind}
 if kind=='file':
  before=old.read_bytes();after=q.read_bytes();assert sha(before)==item['sha256'];m.update(bytes=len(after),sha256=sha(after),file_mode=stat.S_IMODE(q.stat().st_mode));blob=hashlib.sha1(b'blob '+str(len(before)).encode()+b'\0'+before).hexdigest();m['baseline_blob_sha1']=blob
  if before!=after:
   changes.append(r);dest=E/'before'/r;dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(before)
   m['changed_from_baseline']=True
  else:m['changed_from_baseline']=False
  blobs.append((r,item['mode'],blob))
 elif kind=='symlink':
  target=os.readlink(q);assert target==item['target']==os.readlink(old);m.update(target=target,sha256=sha(os.fsencode(target)),changed_from_baseline=False);b=os.fsencode(target);blobs.append((r,'120000',hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()))
 else:
  assert list(q.iterdir())==[] and list(old.iterdir())==[];m.update(gitlink=item['publisher_git_object'],expanded=False,changed_from_baseline=False);blobs.append((r,'160000',item['publisher_git_object']))
 manifest.append(m)
new='reverie-kvm/src/timestamp.rs';q=S/new;b=q.read_bytes();manifest.append({'relative':new,'path':str(q),'mode':'100644','kind':'file','bytes':len(b),'sha256':sha(b),'file_mode':stat.S_IMODE(q.stat().st_mode),'new':True,'changed_from_baseline':True});changes.append(new);manifest.sort(key=lambda x:x['relative'])
def tree(entries):
 root={}
 for path,mode,blob in entries:
  d=root;parts=path.split('/')
  for part in parts[:-1]:d=d.setdefault(part,{})
  d[parts[-1]]=(mode,blob)
 def digest(d):
  items=[]
  for name,v in d.items():
   mode,blob=('40000',digest(v)) if isinstance(v,dict) else v
   items.append((name.encode()+ (b'/' if mode=='40000' else b''),mode.encode()+b' '+name.encode()+b'\0'+bytes.fromhex(blob)))
  b=b''.join(v for _,v in sorted(items));return hashlib.sha1(b'tree '+str(len(b)).encode()+b'\0'+b).hexdigest()
 return digest(root)
t=tree(blobs);assert t=='7620fe83f486d665d9d09d4f09f0e93636b862e4',t
write('BASE-TREE-READBACK.json',{'kind':'offline Git-object hashing of every actual frozen base leaf, without Git/SCM calls','base_commit_reported_by_root':'79516661bf82d30ab2967c71834a6d47447b76ee','root_reported_tree':t,'reconstructed_actual_tree':t,'entries':len(blobs),'source':str(B)})
write('SOURCE-MANIFEST.json',manifest)
patch=''.join(''.join(difflib.unified_diff((B/x).read_text().splitlines(True) if (B/x).exists() else [],(S/x).read_text().splitlines(True),fromfile='a/'+x if (B/x).exists() else '/dev/null',tofile='b/'+x)) for x in sorted(changes));(P/'SOURCE.patch').write_text(patch)
# Historical tests are extracted from retained additions, including their original assertions.
oldstatic=''
for filename in ('841107ac.patch','d65ab382.patch','96306604.patch'):
 content=(E/filename).read_text();section=content.split('diff --git a/reverie-kvm/tests/static_elf.rs b/reverie-kvm/tests/static_elf.rs\n',1)[1];oldstatic+='\n'+'\n'.join(x[1:] for x in section.splitlines() if x.startswith('+') and not x.startswith('+++') and not x.startswith('+use reverie::'))
(P/'evidence/historical-static-additions.rs').write_text(oldstatic)
def body(s,name):
 at=s.index('fn '+name+'(');start=s.index('{',at);depth=0
 for i in range(start,len(s)):
  if s[i]=='{':depth+=1
  if s[i]=='}':
   depth-=1
   if depth==0:return s[at:i+1]
 raise ValueError(name)
historical=['static_elf_timestamp_reads_dispatch_exact_tool_results_repeatably','timestamp_dispatch_survives_thread_fork_and_exec_vcpu_lifecycles','static_elf_unsubscribed_rdtsc_runs_without_tool_dispatch','subscribed_timestamp_dispatch_refuses_unrelated_exceptions','repeated_timestamp_reads_evolve_once_per_instruction','static_elf_unsubscribed_rdtscp_remains_guest_exception'];current=(S/'reverie-kvm/tests/static_elf.rs').read_text();history=[]
for name in historical:
 old=body(oldstatic,name);newbody=body(current,name);same=re.sub(r'\s+','',old)==re.sub(r'\s+','',newbody);assert same,name;history.append({'name':name,'historical_sha256':sha(old.encode()),'current_sha256':sha(newbody.encode()),'byte_identical':old==newbody,'only_whitespace_difference':same,'no_oracle_or_argument_change':True})
write('HISTORICAL-CONTROL-CONTINUITY.json',history)
# Record all old source-prefix preservation and exact unchanged clock/Tool/public-VM paths.
base_static=(B/'reverie-kvm/tests/static_elf.rs').read_text();normal=current.replace('use reverie::Rdtsc;\n','',1).replace('use reverie::RdtscResult;\n','',1);assert normal.startswith(base_static)
for path in ('reverie-kvm/src/clock.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/elf.rs','reverie-kvm/tests/vmcall.rs','reverie-kvm/tests/read_clock.rs','reverie/src/tool.rs','reverie/src/guest.rs'):
 assert (B/path).read_bytes()==(S/path).read_bytes()
write('SOURCE-CONTINUITY.json',{'changed_paths':sorted(changes),'all_other_manifest_leaves_equal_to_base':True,'old_static_file_exact_prefix_after_removing_two_new_imports':True,'no_old_static_test_or_helper_edit':True,'historical_controls':history,'clock_executor_elf_public_Tool_Guest_vmcall_read_clock_bytes_unchanged':True,'base_tree':t,'extra_untracked_lock':rec(S/'Cargo.lock')})
# Own author grounding, plus unchanged Hermit callback context, retained independently of future composition.
H=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918')
contexts=[(H/'ignored/process-publication-design-v4/grounding/OWN-GROUNDING.json','OWN-GROUNDING.json','1cf3add6899eeaf5e8df2f4f7adb82fc962f200d4013a5e8adb8c61042851433'),(H/'ignored/process-exit-progress-investigation-v1/source/hermit/detcore/src/lib.rs','hermit-detcore-lib.rs',None)]
ctx=[]
for origin,name,expected in contexts:
 b=origin.read_bytes()
 if expected:assert sha(b)==expected
 dest=E/name;dest.write_bytes(b);ctx.append({'source':str(origin),'retained':rec(dest),'purpose':'own author grounding (prior exposure disclosed)' if name=='OWN-GROUNDING.json' else 'unchanged callback/time-charge source context; not a qualified new Hermit composition'})
write('SOURCE_INPUTS.json',{'base_tree':rec(P/'BASE-TREE-READBACK.json'),'baseline_manifest':rec(P/'BASELINE-MANIFEST.json'),'candidate_manifest':rec(P/'SOURCE-MANIFEST.json'),'contexts':ctx,'historical_inputs':[rec(E/n) for n in ('841107ac.patch','d65ab382.patch','96306604.patch','kvm-rdtsc-prior-pr403-comments-20260918.json')],'parity_summary':rec(E/'parity/SUMMARY.json'),'author_disclosure':'This worker authored the candidate and prior scheduler designs. This packet is not an independent review; historical source/runtime review credit is not transferred.'})
print({'tree':t,'changes':len(changes),'entries':len(manifest),'patch_bytes':len(patch.encode()),'patch_lines':len(patch.splitlines()),'historical_bodies_byte_equal':sum(x['byte_identical'] for x in history)})
