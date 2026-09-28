from pathlib import Path
import collections,difflib,hashlib,json,os,re,shutil,stat
N=Path(__file__).resolve().parent
R=N.parents[1]; B=N.parent/'rdtsc-recovery-source-v6'; S=N/'source'
SUP=R.parent/'kvm-parent-reader-support-20260916'
H=R.parent/'kvm-replay-prerequisites-20260918'
def sha(data):return hashlib.sha256(data).hexdigest()
def file(p):
 p=Path(p);assert p.is_file() and not p.is_symlink(),p
 data=p.read_bytes();return {'path':str(p),'kind':'file','bytes':len(data),'sha256':sha(data),'mode':stat.S_IMODE(p.stat().st_mode)}
def write(name,value):(N/name).write_text(json.dumps(value,indent=2)+'\n')
base=json.loads((B/'SOURCE-MANIFEST.json').read_text()); manifest=[]; changed=[]; new=[]; oldrows=[]
for row in base:
 rel=row['relative'];p=S/rel;q=B/'source'/rel;kind=row['kind']
 if kind=='file':
  old=file(q);assert old['sha256']==row['sha256'] and old['bytes']==row['bytes'],q
  rec={'relative':rel,**file(p),'base_sha256':old['sha256']};rec['changed_from_v6']=rec['sha256']!=old['sha256']
  assert rec['mode']==old['mode'],rel
  if rec['changed_from_v6']:changed.append(rel)
  oldrows.append(old)
 elif kind=='symlink':
  assert q.is_symlink() and p.is_symlink();a=os.readlink(q);b=os.readlink(p);assert a==b==row['target'];assert sha(a.encode())==row['sha256']
  rec={'relative':rel,'path':str(p),'kind':kind,'target':b,'sha256':sha(b.encode()),'changed_from_v6':False}
  oldrows.append({'path':str(q),'kind':kind,'target':a,'sha256':sha(a.encode())})
 else:
  assert kind=='unexpanded_gitlink' and p.is_dir() and q.is_dir() and not list(p.iterdir()) and not list(q.iterdir())
  rec={'relative':rel,'path':str(p),'kind':kind,'gitlink':row['gitlink'],'expanded':False,'changed_from_v6':False}
  oldrows.append({'path':str(q),'kind':kind,'gitlink':row['gitlink'],'expanded':False})
 manifest.append(rec)
newpaths=['reverie-kvm/src/cpuid_instruction.rs','reverie-kvm/src/cpuid_runtime_tests.rs','reverie-kvm/tests/support/cpuid_dispatch.rs','reverie-kvm/tests/support/cpuid_terminal.rs']
for rel in newpaths:
 assert not (B/'source'/rel).exists();manifest.append({'relative':rel,**file(S/rel),'changed_from_v6':True,'new':True});new.append(rel)
assert sorted(changed)==sorted(['reverie-kvm/src/lib.rs','reverie-kvm/src/vm.rs','reverie-kvm/src/runtime.rs','reverie-kvm/tests/static_elf.rs'])
expected={x['relative'] for x in manifest}|{'Cargo.lock'}; actual=set()
for root,dirs,files in os.walk(S,followlinks=False):
 for d in list(dirs):
  p=Path(root)/d
  if p.is_symlink() or any(p==S/x['relative'] and x['kind']=='unexpanded_gitlink' for x in manifest):actual.add(str(p.relative_to(S)));dirs.remove(d)
 for f in files:actual.add(str((Path(root)/f).relative_to(S)))
assert actual==expected,(actual-expected,expected-actual)
lock=file(S/'Cargo.lock');oldlock=file(B/'source/Cargo.lock');assert lock['sha256']==oldlock['sha256']
write('SOURCE-MANIFEST.json',sorted(manifest,key=lambda x:x['relative']))
# Test bodies in existing files are protected byte-for-byte.
old=(B/'source/reverie-kvm/tests/static_elf.rs').read_text();now=(S/'reverie-kvm/tests/static_elf.rs').read_text()
insert='#[path = "support/cpuid_dispatch.rs"]\nmod cpuid_dispatch;\n\n#[path = "support/cpuid_terminal.rs"]\nmod cpuid_terminal;\n\n'
assert now.count(insert)==1 and now.replace(insert,'')==old
oldvm=(B/'source/reverie-kvm/src/vm.rs').read_text().split('#[cfg(test)]\nmod tests {',1)[1]
newvm=(S/'reverie-kvm/src/vm.rs').read_text().split('#[cfg(test)]\nmod tests {',1)[1]
assert newvm.replace('\n    include!("cpuid_runtime_tests.rs");\n','',1)==oldvm
oldrt=(B/'source/reverie-kvm/src/runtime.rs').read_text().split('#[cfg(test)]\nmod tests {',1)[1]
newrt=(S/'reverie-kvm/src/runtime.rs').read_text().split('#[cfg(test)]\nmod tests {',1)[1]
assert oldrt==newrt
patch=[]
for rel in sorted(changed+new):
 old=(B/'source'/rel).read_text().splitlines(True) if rel not in new else []
 now=(S/rel).read_text().splitlines(True)
 patch.append(f'diff --git a/{rel} b/{rel}\n')
 if rel in new:patch.append('new file mode 100644\n')
 patch.extend(difflib.unified_diff(old,now,fromfile='/dev/null' if rel in new else f'a/{rel}',tofile=f'b/{rel}'))
(N/'SOURCE.patch').write_text(''.join(patch))
write('SOURCE-CONTINUITY.json',{'base':file(B/'TARGET.json'),'qualified_base':file(B/'final-v1/TARGET.json'),'entries':len(manifest),'kinds':dict(collections.Counter(x['kind'] for x in manifest)),'changed_existing':changed,'new_paths':new,'unchanged_existing_test_bodies':{'static_elf_remove_two_module_declarations_recovers_complete_file':True,'vm_remove_one_test_include_recovers_complete_test_module':True,'runtime_complete_test_suffix_identical':True},'fixed_cpuid_policy_unchanged':file(S/'reverie-kvm/src/cpuid.rs'),'public_reverie_api_unchanged':True,'cargo_manifest_and_dependencies_unchanged':True,'actual_lock':lock,'base_lock':oldlock,'experimental_probe_absent':not (S/'reverie-kvm/src/cpuid_fault_probe.rs').exists(),'source_only':True})
# Keep current Hermit context as retained copies, not mutable worktree references.
context=N/'context';context.mkdir(exist_ok=True);origins=[]
for rel in ['detcore/src/lib.rs','detcore/src/cpuid.rs','detcore-model/src/time.rs']:
 p=H/'ignored/rdtsc-h39ac-cancellation-v1/composition/hermit'/rel;q=context/'hermit-h39ac'/rel;q.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(p,q);origins.append({'origin':file(p),'copy':file(q)})
write('CONTEXT-ORIGINS.json',{'hermit':'H39ac source composition retained with V6 timestamp witness build; no new build','records':origins})
# Exact new declaration inventory, followed by complete unchanged V6 selection.
new_specs=[
 ('lib','reverie-kvm/src/cpuid_instruction.rs','cpuid_instruction::tests::','Pure decoder/full-register assertions; actual MSR tests require real KVM and exact error/readback values'),
 ('lib','reverie-kvm/src/cpuid_runtime_tests.rs','vm::tests::cpuid_runtime_controls::','Real CPL3 fetch-fault priority and prior-owned-state disarm on actual direct/public/ELF loops'),
 ('static','reverie-kvm/tests/support/cpuid_dispatch.rs','cpuid_dispatch::','Real Tool CPUID outputs/faults/clocks, Host/Tool ownership, fork/exec, table policy and mixed-instruction dispatch'),
 ('static','reverie-kvm/tests/support/cpuid_terminal.rs','cpuid_terminal::','Real ordinary/tail terminal success, consuming cleanup, unsupported-tail refusal and sibling ordering; no Detcore cancellation claim')]
groups={};controls=[]
for artifact,rel,prefix,purpose in new_specs:
 text=(S/rel).read_text()
 for match in re.finditer(r'#\[test\]\s+fn (\w+)',text):
  name=prefix+match.group(1);phase=f'cpuid-{len(groups)+1:02d}';item={'artifact':artifact,'names':[name],'source':rel,'origin':'new unexecuted CPUID candidate','purpose':purpose};groups[phase]=item
  controls.append({'phase':phase,'artifact':artifact,'name':name,'source':file(S/rel),'line':text[:match.start()].count('\n')+1,'origin':item['origin'],'purpose':purpose,'executed':False,'cpu_seconds':30,'wall_seconds':60})
assert len(groups)==15
previous=json.loads((B/'qualification-v1/SELECTORS.json').read_text())
for phase,item in previous['groups'].items():
 item=dict(item);item['origin']='unchanged V6 neighbor; historical V6 result is not candidate execution';groups[phase]=item
for phase,artifact,rel,name in [
 ('cpuid-neighbor-01','lib','reverie-kvm/src/cpuid.rs','cpuid::tests::deterministic_xstate_subleaves_are_indexed_and_unique'),
 ('cpuid-neighbor-02','lib','reverie-kvm/src/cpuid.rs','cpuid::tests::deterministic_cpuid_table_rejects_duplicate_function_index'),
 ('cpuid-neighbor-03','static','reverie-kvm/tests/static_elf.rs','dynamic_c_guest_observes_indexed_xstate_cpuid_and_lazy_binding')]:
 groups[phase]={'artifact':artifact,'names':[name],'source':rel,'origin':'unchanged CPUID/XSAVE neighbor','purpose':'Preserve installed table/indexed XSAVE and actual dynamic guest behavior; existing oracle unchanged'}
for phase,item in list(groups.items())[15:]:
 rel=item['source'];text=(S/rel).read_text();name=item['names'][0];match=re.search(r'fn '+re.escape(name.rsplit('::',1)[-1])+r'\b',text);assert match,name
 controls.append({'phase':phase,'artifact':item['artifact'],'name':name,'source':file(S/rel),'line':text[:match.start()].count('\n')+1,'origin':item['origin'],'purpose':item['purpose'],'executed':False,'cpu_seconds':30,'wall_seconds':60})
assert len(groups)==len(controls)==55 and len({(c['artifact'],c['name']) for c in controls})==55
write('SELECTORS.json',{'groups':groups,'exact_declarations':55,'actual_inventory_pending':True})
write('CONTROLS.json',{'new_declarations':15,'unchanged_v6_declarations':37,'additional_unchanged_declarations':3,'declarations':55,'execution_count':0,'actual_inventory_pending':True,'mode_count_not_inferred_from_declarations':True,'controls':controls})
# Bind measured transport/primary records without claiming their own binary tested this source.
prereqs=[B/'TARGET.json',B/'SOURCE-MANIFEST.json',B/'final-v1/TARGET.json',B/'final-v1/REPORT.md',B/'qualification-v1/SELECTORS.json',B/'qualification-v1/SETUP.json',B/'qualification-v1/metadata-plan.json',B/'qualification-v1/compile-plan.json']
P=N.parent/'cpuid-transport-probe-v1'
prereqs += [P/'final-v1'/x for x in ['TARGET.json','REPORT.md','HARDWARE-OBSERVATIONS.json','INPUTS.json','READBACK.json']]
prereqs += [P/'qualification-result-v1'/x for x in ['RESULTS.json','RAW-EXECUTION-AUDIT.json']]
prereqs += [P/'qualification-v1/observer/probe-01'/x for x in ['stdout','stderr','result.json']]
prereqs += [P/'qualification-v1/controls/probe-01/result.json',P/'qualification-v1/probe-01-plan.json',P/'qualification-v1/retained-binaries/BINDING.json']
for directory in [SUP/'ignored/kvm-cpuid-transport-research-v1',SUP/'ignored/kvm-cpuid-feature-query-v1']:
 prereqs += sorted(p for p in directory.iterdir() if p.is_file() and not p.is_symlink())
prereqrecords=[file(p) for p in prereqs];write('PREREQUISITES.json',{'role':'source and measured transport prerequisites; no candidate execution or inherited review approval','records':prereqrecords})
# Full patch/new source/control documents in ordinary bounded chunks for later review preparation.
chunkdir=N/'read-chunks';chunkdir.mkdir(exist_ok=True);docs=[];chunks=[]
for label,path in [('report',N/'REPORT.md'),('plan',N/'PLAN.md'),('patch',N/'SOURCE.patch')]+[(Path(rel).stem,S/rel) for rel in new]:
 lines=path.read_text().splitlines(keepends=True);doc={'label':label,'file':file(path),'lines':len(lines),'chunks':[]}
 for start in range(0,len(lines),80):
  end=min(start+80,len(lines));q=chunkdir/f'{label}-{start+1:04d}-{end:04d}.txt';q.write_text(''.join(lines[start:end]));rec={'source':str(path),'first_line':start+1,'last_line':end,**file(q)};doc['chunks'].append(rec);chunks.append(rec)
 docs.append(doc)
write('COMPLETE_READS.json',{'ordinary_source_reads_only':True,'reviewer_not_launched':True,'documents':docs,'chunks':len(chunks),'lines':sum(d['lines'] for d in docs)})
records={}
def add(rec):
 rec={k:v for k,v in rec.items() if k in ['path','kind','bytes','sha256','mode','target','gitlink','expanded']}
 if 'kind' not in rec:rec['kind']='file'
 assert rec['path'] not in records or records[rec['path']]==rec,rec['path']
 records[rec['path']]=rec
for rec in manifest+oldrows+prereqrecords+[lock,oldlock]:add(rec)
for row in origins:add(row['origin']);add(row['copy'])
for rec in chunks:add(rec)
for p in sorted(N.iterdir()):
 if p.is_file() and p.name not in ['INPUTS.json','TARGET.json','READBACK.json']:add(file(p))
for p in sorted((N/'preparation').iterdir()):add(file(p))
write('INPUTS.json',{'authorship':'kvm_prerequisite_impl author; no independent source verdict','source_only':True,'records':list(records.values())})
write('TARGET.json',{'status':'FROZEN ISOLATED CPUID SOURCE V1; UNCOMPILED AND UNEXECUTED','base':file(B/'TARGET.json'),'qualified_base':file(B/'final-v1/TARGET.json'),'source':str(S),'source_manifest':file(N/'SOURCE-MANIFEST.json'),'source_patch':file(N/'SOURCE.patch'),'continuity':file(N/'SOURCE-CONTINUITY.json'),'actual_lock':lock,'report':file(N/'REPORT.md'),'plan':file(N/'PLAN.md'),'selectors':file(N/'SELECTORS.json'),'controls':file(N/'CONTROLS.json'),'prerequisites':file(N/'PREREQUISITES.json'),'input_index':file(N/'INPUTS.json'),'complete_reads':file(N/'COMPLETE_READS.json'),'new_declarations':15,'planned_declarations':55,'planned_phases':64,'compiler_run':False,'tests_run':False,'source_approved':False,'live_source_scm_or_cache_changed':False,'review_launch_authorized':False})
# Authenticate every actual record, including aliases and intentionally empty gitlinks.
for rec in records.values():
 p=Path(rec['path'])
 if rec['kind']=='file':assert file(p)==rec,(p,file(p),rec)
 elif rec['kind']=='symlink':assert p.is_symlink() and os.readlink(p)==rec['target'] and sha(os.readlink(p).encode())==rec['sha256'],p
 else:assert rec['kind']=='unexpanded_gitlink' and p.is_dir() and not list(p.iterdir()),p
write('READBACK.json',{'target':file(N/'TARGET.json'),'inputs':file(N/'INPUTS.json'),'records_authenticated':len(records),'kind_counts':dict(collections.Counter(x['kind'] for x in records.values())),'source_entries':len(manifest),'changed_paths':len(changed)+len(new),'new_test_declarations':15,'planned_exact_declarations':55,'preparation_format_status':0,'compile_or_test_execution':False,'all_records_stable':True,'no_independent_approval':True})
print(json.dumps({name:file(N/name)['sha256'] for name in ['TARGET.json','SOURCE.patch','SOURCE-MANIFEST.json','REPORT.md','PLAN.md','CONTROLS.json','INPUTS.json','READBACK.json']},indent=2));print('bound',len(records),'source',len(manifest),'chunks',len(chunks))
