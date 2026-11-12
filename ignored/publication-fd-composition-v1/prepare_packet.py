"""Source-only packet construction; no formatter, compiler, tests or lease access."""
from pathlib import Path
import difflib, hashlib, json, os, shutil
D=Path(__file__).resolve().parent
R=D.parents[1]
P=R/'ignored/process-publication-implementation-v1/frozen-v1'
F=R/'ignored/same-inode-ofd-repair-v2/final-v3'
PQ=R/'ignored/process-publication-implementation-v1/qualification-v2'
FQ=R/'ignored/same-inode-ofd-repair-v2/qualification-v3'
paths=['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/process_signal_publication.rs']
def record(p):
 b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}
def write(name,x):
 p=D/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(json.dumps(x,indent=2)+'\n')
for rel in paths:
 p=D/'after'/rel;p.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(D/'source'/rel,p)
for oldroot,name in [(P/'source','PUBLISHER-TO-COMPOSED.patch'),(F/'after','FD-TO-COMPOSED.patch'),(D/'base','SOURCE.patch')]:
 out=''
 for rel in paths:
  a=(oldroot/rel).read_text() if (oldroot/rel).is_file() else ''
  b=(D/'source'/rel).read_text()
  out+=''.join(difflib.unified_diff(a.splitlines(keepends=True),b.splitlines(keepends=True),fromfile='a/'+rel if a else '/dev/null',tofile='b/'+rel))
 (D/name).write_text(out)
manifest=[]
for row in json.loads((P/'FULL-SOURCE-MANIFEST.json').read_text()):
 rel=row['path'];p=D/'source'/rel
 item={'relative':rel,'mode':row['mode'],'publisher_git_object':row.get('git_object')}
 if row['mode']=='160000':
  assert p.is_dir() and not list(p.iterdir());item['kind']='unexpanded_gitlink'
 elif row['mode']=='120000':
  item.update(kind='symlink',target=os.readlink(p),sha256=hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest())
  assert item['sha256']==row['sha256']
 else:
  item.update(kind='file',**record(p))
  if rel not in paths[:2]:assert item['sha256']==row['sha256'],rel
 manifest.append(item)
write('SOURCE-MANIFEST.json',manifest)
write('qualification-proposal/source-manifest.json',[{'path':x['relative'],'mode':x['mode'],**({'git_object':x['publisher_git_object']}if x['kind']=='unexpanded_gitlink'else{'sha256':x['sha256']})}for x in manifest])
write('SOURCE_INPUTS.json',{'base':json.loads((P/'TARGET.json').read_text())['base'],'publisher_target':record(P/'TARGET.json'),'publisher_manifest':record(P/'FULL-SOURCE-MANIFEST.json'),'fd_final_inputs':record(F/'SOURCE_INPUTS.json'),'fd_final_readback':record(F/'READBACK.json'),'after':[{'relative':x,'file':record(D/'after'/x)} for x in paths],'full_manifest':record(D/'SOURCE-MANIFEST.json'),'actual_ignored_lock':record(D/'source/Cargo.lock'),'execution_performed':False})

pub=json.loads((PQ/'SELECTORS.json').read_text());fd=json.loads((FQ/'SELECTORS.json').read_text())
seen={(v['artifact'],n) for v in pub['groups'].values()for n in v['names']}
groups=dict(pub['groups']);deduplicated=[]
for key,value in fd['groups'].items():
 names=[n for n in value['names'] if (value['artifact'],n)not in seen]
 deduplicated +=[{'artifact':value['artifact'],'name':n}for n in value['names']if(value['artifact'],n)in seen]
 if names:groups['fd-'+key.removeprefix('test-')]={'artifact':value['artifact'],'names':names}
 for n in names:seen.add((value['artifact'],n))
write('qualification-proposal/SELECTORS.json',{'groups':groups,'proposed_unique_declarations':len(seen),'proposed_lib_declarations':sum(x[0]=='lib'for x in seen),'proposed_static_declarations':sum(x[0]=='static'for x in seen),'deduplicated':deduplicated,'inputs':[record(PQ/'SELECTORS.json'),record(FQ/'SELECTORS.json')],'execution_performed':False})
write('qualification-proposal/SETUP.json',{'owner_slot':str(R),'source_root':str(D/'source'),'base':json.loads((P/'TARGET.json').read_text())['base'],'source_files':[{'relative':rel,'file':record(D/'after'/rel)}for rel in paths],'purpose':'Proposed isolated inactive publisher plus descriptor-entry repair composition; unexecuted'})
caller=(PQ/'prepare.py').read_text()
needle="Path('/usr/bin/ld').resolve()]"
assert caller.count(needle)==1;caller=caller.replace(needle,"Path('/usr/bin/ld').resolve(),Path('/usr/bin/timeout')]")
start=caller.index("        actual=subprocess.check_output(")
end=caller.index("        argv=[str(TOOL/'rustfmt')",start)
fdcaller=(FQ/'prepare.py').read_text()
a=fdcaller.index("        require(REPO != OWNER")
b=fdcaller.index("        argv=[str(TOOL/'rustfmt')",a)
caller=caller[:start]+fdcaller[a:b]+caller[end:]
caller=caller.replace("scope='Inactive process publication prerequisite; new private unit controls and unchanged existing VM consumers. No scheduler activation or Hermit timer repair claim'", "scope='Inactive publisher and descriptor-entry composition; unchanged unit and VM controls. No activation, FIFO or timer completion claim'")
(D/'qualification-proposal/prepare.py').write_text(caller)
(D/'CALLER-CHANGE.patch').write_text(''.join(difflib.unified_diff((PQ/'prepare.py').read_text().splitlines(keepends=True),caller.splitlines(keepends=True),fromfile='publisher-qualification-v2/prepare.py',tofile='qualification-proposal/prepare.py')))
closure=[]
for p in sorted(PQ.glob('*.py'))+sorted((PQ/'observer').glob('*.py')):
 peer=FQ/p.relative_to(PQ)
 item={'publisher':record(p),'fd':record(peer)if peer.is_file()else None,'unchanged_between_components':peer.is_file()and p.read_bytes()==peer.read_bytes()}
 closure.append(item)
write('CALLER_INPUTS.json',{'original_closure':closure,'observer_source_inputs':record(PQ/'observer/source-inputs.json'),'proposed_prepare':record(D/'qualification-proposal/prepare.py'),'proposed_setup':record(D/'qualification-proposal/SETUP.json'),'proposed_selectors':record(D/'qualification-proposal/SELECTORS.json'),'proposed_source_manifest':record(D/'qualification-proposal/source-manifest.json'),'execution_performed':False,'not_a_deployed_caller':True})
mark='#[cfg(test)]\nmod tests {'
ct=(D/'source/reverie-kvm/src/executor.rs').read_text().split(mark)
ft=(F/'after/reverie-kvm/src/executor.rs').read_text().split(mark)
assert len(ct)==len(ft)==2 and ct[1]==ft[1]
assert (D/'source/reverie-kvm/src/process_signal_publication.rs').read_bytes()==(P/'source/reverie-kvm/src/process_signal_publication.rs').read_bytes()
assert (D/'source/reverie-kvm/tests/static_elf.rs').read_bytes()==(P/'source/reverie-kvm/tests/static_elf.rs').read_bytes()
write('TEST-SOURCE-CONTINUITY.json',{'executor_tests_module':{'identical_to_fd_final_v3':True,'bytes':len(ct[1].encode()),'sha256':hashlib.sha256(ct[1].encode()).hexdigest()},'publisher_module':{'unchanged':True,**record(D/'source/reverie-kvm/src/process_signal_publication.rs')},'static_harness':{'unchanged':True,**record(D/'source/reverie-kvm/tests/static_elf.rs')},'all_other_source_entries':{'unchanged_from_publisher_except_two_composed_files':True,'manifest_count':len(manifest)},'unique_proposed_selected_declarations':len(seen),'execution_performed':False})
print(json.dumps({'manifest_entries':len(manifest),'selectors':len(seen),'source':record(D/'SOURCE.patch'),'delta':record(D/'PUBLISHER-TO-COMPOSED.patch'),'caller_delta':record(D/'CALLER-CHANGE.patch')},indent=2))
