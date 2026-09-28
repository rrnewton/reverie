from pathlib import Path
import json,hashlib,subprocess,difflib,os,fcntl
R=Path(__file__).resolve().parents[2];D=Path(__file__).resolve().parent;Q=D/'qualification-v3';S=D/'corrected-source-v3';F=D/'final-v3';F.mkdir()
def rec(p):
 p=Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':hashlib.sha256(b).hexdigest()}
def write(p,value):
 with p.open('x')as f:f.write(json.dumps(value,indent=2)+'\n')
base='000c15a1161ea2d58749431b5ddaaa97f7aa37d5';paths=['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs'];patch=''
for rel in paths:
 b=subprocess.check_output(['git','show',base+':'+rel],cwd=R,timeout=30);a=(S/rel).read_bytes();p=F/'before'/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(b);p=F/'after'/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(a);patch+=''.join(difflib.unified_diff(b.decode().splitlines(True),a.decode().splitlines(True),fromfile='a/'+rel,tofile='b/'+rel))
(F/'SOURCE.patch').write_text(patch)
manifest=json.loads((Q/'source-manifest.json').read_text());assert len(manifest)==2620
for x in manifest:
 p=S/x['path']
 if x['mode']=='160000':continue
 if x['mode']=='120000':assert hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest()==x['sha256']
 else:assert rec(p)['sha256']==x['sha256'] and bool(p.stat().st_mode&0o111)==(x['mode']=='100755')
assert not(S/'reverie-kvm/src/process_signal_publication.rs').exists()
selection=json.loads((Q/'SELECTORS.json').read_text())['groups'];names=list(selection);phase_names=['metadata','compile','format','core-check','clippy','list-lib',*names];assert len(phase_names)==21
results=[]
for name in phase_names:
 p=Q/(name+'-plan.json');c=Q/'controls'/name/'result.json';o=Q/'observer'/name;result=json.loads(c.read_text());observed=json.loads((o/'result.json').read_text());plan=json.loads(p.read_text())
 assert result['accepted']and result['terminal_authenticated']and result['inputs_unchanged']and result['raw_status']==0 and result['plan_sha256']==rec(p)['sha256']
 assert observed['accounting_complete']and observed['final_accounting']['cgroup_empty']
 assert plan['limits']=={'aggregate_cpu_usec':600000000 if name in ['metadata','compile','core-check','clippy']else 30000000,'wall_seconds':900 if name in ['metadata','compile','core-check','clippy']else 60,'lethal_stderr_bytes':16777216,'live_stdout_samples_bytes':67108864,'phase_read_bytes':16777216,'memory_bytes':17179869184,'swap_bytes':0,'free_floor_bytes':107374182400}
 if name in selection:
  rows=[json.loads(line)for line in (o/'stdout').read_text().splitlines()];assert [x['name']for x in rows if x.get('event')=='started'and x.get('type')=='test']==selection[name]['names'];ends=[x for x in rows if x.get('event')in('ok','failed','ignored')and x.get('type')=='test'];assert len(ends)==1 and ends[0]['event']=='ok';suite=[x for x in rows if x.get('event')in('ok','failed')and x.get('type')=='suite'];assert len(suite)==1 and suite[0]['passed']==1 and suite[0]['failed']==suite[0]['ignored']==suite[0]['measured']==0
 results.append({'name':name,'plan':rec(p),'result':rec(c),'observer':rec(o/'result.json'),'stdout':rec(o/'stdout'),'stderr':rec(o/'stderr'),'raw':0,'accepted':True,'cpu_seconds':observed['final_accounting']['cpu_usage_nsec']/1e9,'observed_wall_seconds':observed['elapsed_seconds'],'payload_seconds':result['payload_exit']['elapsed_seconds'],'readback':result['readback']})
assert json.loads((Q/'controls/list-lib/result.json').read_text())['readback']['count']==490
emfile=(Q/'observer/test-emfile/stderr').read_text();assert 'isolated file-table control status=exit status: 0' in emfile and emfile.splitlines().count('file-table EMFILE control completed')==1
old=D/'qualification';failed=json.loads((old/'controls/format/result.json').read_text());assert not failed['accepted']and failed['terminal_authenticated']and failed['inputs_unchanged']and failed['raw_status']==1
write(F/'RESULTS.json',{'source_manifest':rec(Q/'source-manifest.json'),'source_patch':rec(F/'SOURCE.patch'),'phases':results,'selected':15,'passed':15,'failed':0,'ignored':0,'actual_lib_inventory':490,'corrected_phase_count':21,'total_cpu_seconds':sum(x['cpu_seconds']for x in results),'total_observed_wall_seconds':sum(x['observed_wall_seconds']for x in results),'test_payload_seconds':sum(x['payload_seconds']for x in results if x['name']in selection),'first_attempt':{'metadata':rec(old/'controls/metadata/result.json'),'compile':rec(old/'controls/compile/result.json'),'retained_elf':rec(old/'retained/BINDING.json'),'format_failure':rec(old/'controls/format/result.json'),'format_diff':rec(old/'observer/format/stdout'),'raw':1,'accepted':False},'retained_corrected_elf':rec(Q/'retained/BINDING.json'),'emfile_child_status':0,'emfile_completion_markers':1})
# Exact current publisher preservation; its frozen reviews are independent.
pub=R/'ignored/process-publication-implementation-v1/frozen-v1';records=[]
for rel in ['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/process_signal_publication.rs']:
 assert(R/rel).read_bytes()==(pub/'source'/rel).read_bytes();records.append({'live_at_readback':rec(R/rel),'frozen':rec(pub/'source'/rel)})
identity=json.loads((pub/'SOURCE_INPUTS.json').read_text());head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=R,timeout=30).decode().strip();index=hashlib.sha256(subprocess.check_output(['git','ls-files','--stage'],cwd=R,timeout=30)).hexdigest();assert head==identity['base']and index==identity['index_sha256']
write(F/'LIVE-PUBLISHER-CONTINUITY.json',{'head':head,'index_sha256':index,'three_live_product_files_equal_frozen':records,'no_live_product_or_SCM_edits_by_this_qualification':True})
lease=R/'ignored/timer-integration-20260918/lane.lease';fd=os.open(lease,os.O_RDWR|os.O_CLOEXEC);fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);st=os.fstat(fd);fcntl.flock(fd,fcntl.LOCK_UN);os.close(fd)
write(F/'LEASE-RELEASE.json',{'path':str(lease),'identity':[st.st_dev,st.st_ino,st.st_uid],'exclusive_availability_verified':True,'descriptor_closed':True,'last_terminal_phase':rec(Q/'controls/test-emfile/result.json'),'no_lease_state_mutation':True})
peer=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/reverie-same-inode-ofd-repair-v2')
write(F/'SOURCE_INPUTS.json',{'base':base,'reviewed_original_packet':rec(peer/'READBACK.json'),'reviewed_production_patch':rec(peer/'production.patch'),'reviewed_combined_patch':rec(peer/'combined.patch'),'format_only_delta':rec(D/'successor-v3/FORMAT-ONLY.patch'),'snapshot':str(S),'full_manifest':rec(Q/'source-manifest.json'),'after':[rec(F/'after'/p)for p in paths],'original_baseline':rec(peer/'BASELINE.json'),'actual_lock':rec(S/'Cargo.lock'),'deployment':rec(D/'DEPLOYMENT.json'),'declaration_count':15,'no_publisher_source_composition':True})
by={x['name']:x for x in results};testrows=[x for x in results if x['name']in selection]
report=f'''The isolated descriptor-entry repair passes all15 exact native library declarations, including the unchanged same-inode regression and the real EMFILE refusal/recovery control. Format, core/ptrace check, Clippy -D warnings, compilation and actual enumeration also passed. This qualifies only the isolated reviewed repair plus formatting-only successor; the live inactive publisher remains byte-identical and no SCM changes occurred.

Source: exact000c15a base plus peer's reviewed combined586e8b9f patch, followed by FORMAT-ONLY.patch0146c1fc (one line wrap in the EMFILE test). Production remains the independently reviewed a429cb5b repair; no assertion/caller/budget/selector was changed. SOURCE.patch and the two complete afterfiles bind the actual v3 source; all2,620 snapshot entries and ignored lock are authenticated. No private-publication module is present in this snapshot.

The original unchanged-production baseline still fails raw101 at (1,32770,1,[49],2,2,32770) versus required(5,33794,1,[53],6,1,32770). The exact original tuple assertion and both same-inode checks now pass on this corrected ELF. The15 selected declarations have15 starts/15 successes/zero failures/ignored; actual full library inventory is490. The nested EMFILE child is part of one declaration, not a sixteenth test. It exited0 and emitted exactly one final completion marker after actual EMFILE, unchanged original handles/flags/offsets/metadata, real epoll operations, and successful subsequent installation. Assertions inside that exact child establish these facts; no simulated resource refusal or original-install mutant was used.

Actual costs: corrected compile{by['compile']['cpu_seconds']:.3f} aggregate CPU s/{by['compile']['observed_wall_seconds']:.3f} observer wall s;15 tests{sum(x['cpu_seconds']for x in testrows):.3f} CPU s/{sum(x['observed_wall_seconds']for x in testrows):.3f} observer wall s, with{sum(x['payload_seconds']for x in testrows):.4f}s total test payload time. Core check{by['core-check']['cpu_seconds']:.3f}/{by['core-check']['observed_wall_seconds']:.3f}s and Clippy{by['clippy']['cpu_seconds']:.3f}/{by['clippy']['observed_wall_seconds']:.3f}s. All21 corrected phases qualified. The earlier v2 metadata and compile passed, then format failed raw1/accepted=false at the one wrapped binding; its snapshot, diff, receipts and separately retained ELF remain unchanged. Total24 phase attempts across these two FD snapshots:23 qualified and one format failure. The historical negative-before run is separate.

Original limits remain exact per phase:600CPU/900wall for metadata/compile/core/Clippy;30CPU/60wall for format/list/each test;16GiB memory/zero swap,16MiB stderr,64MiB maintained stdout,16MiB readback,100GiB freefloor. The new child retains its reviewed10-second timeout plus2-second kill allowance inside those limits. Datednightly2026-07-29/offline/locked/two jobs, observer137c and the unchanged lease/parser are bound. Fresh metadata and dependency walk hash7,083 external files; real Cargo-selected ELF21c2680603757b4fed6f5b91371db48d9114da0f65608146da33a2400db36a68 is retained at qualification-v3/retained/reverie-kvm-lib with a distinct inode and loader/list provenance. The original v2 ELF05b922d8 and publisher ELFs remain separately retained before cache reuse.

Later composition requirement: FD LoadedStaticElf::insert_file currently returns() and drops a replaced File immediately. The inactive publisher deliberately returns/retains replaced descriptors and tables so final drops occur after signal_transaction is released. A composed implementation must preserve that retired-file handoff in dup replacement and preserve exit/exec cleanup ordering; mechanically substituting insert_file under the transaction would lose it. This report grants no composition approval and does not change the live publisher. General allocation panics, post-effect table-publication expect, external OFD exclusivity, managed FIFO, cancellation and timer/scheduler activation remain outside this result.

Goalposts: all original assertions and14 selections retained, one additive fault declaration; no skip, tolerance, comparator, limit, label or guard relaxation. Initial format failure is not relabelled. Only the isolated whitespace successor changed after that failure. Source-only caller setup initially read the peer readback object as an array and stopped before destination creation; corrected schema then authenticated every record. DEPLOYMENT-CLARIFICATION.json distinguishes the initial copied-template origin record from its later declared relocation data. No helper/parser code changed for those preparation corrections.

Lease release/availability is authenticated; all phase descriptors are closed. Independent source reviews of the inactive publisher continue against their unchanged packet. Root retains composition/SCM/landing authority.
'''
(F/'REPORT.md').write_text(report)
records={}
for root in [D/'corrected-source',S,D/'qualification',Q,D/'successor-v3',F]:
 for p in sorted(root.rglob('*')):
  if any(x in('tmp','cache')for x in p.relative_to(root).parts):continue
  if p.is_file()and not p.is_symlink():records[str(p)]=rec(p)
for p in [D/'DEPLOYMENT.json',D/'DEPLOYMENT-CLARIFICATION.json',D/'finalize.py',peer/'READBACK.json',peer/'BASELINE.json']:
 records[str(p)]=rec(p)
write(F/'INPUTS.json',list(records.values()))
write(F/'READBACK.json',[rec(F/n)for n in ['REPORT.md','RESULTS.json','SOURCE.patch','SOURCE_INPUTS.json','LIVE-PUBLISHER-CONTINUITY.json','LEASE-RELEASE.json','INPUTS.json']])
print(json.dumps({n:rec(F/n)for n in ['REPORT.md','RESULTS.json','SOURCE.patch','SOURCE_INPUTS.json','READBACK.json']},indent=2))
