from pathlib import Path
import json,hashlib,subprocess,shutil,difflib,os
r=Path(__file__).resolve().parents[2];d=Path(__file__).resolve().parent;q=d/'qualification-v1';old=r/'ignored/scalar-write-fd-qualification-v1/corrected'
q.mkdir();(q/'observer').mkdir()
for name in ['tmp','cache','controls','loader']:(q/name).mkdir()
def rec(p):return {'path':str(p),'bytes':p.stat().st_size,'mode':p.stat().st_mode&0o777,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
origins=[]
for n in ['phase.py','common.py','cache_lease.py','prepare.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json']:
 shutil.copyfile(old/n,q/n);origins.append({'original':rec(old/n),'copy':rec(q/n)})
s=(q/'prepare.py').read_text();s=s.replace("paths=['reverie-kvm/src/executor.rs', 'reverie-kvm/src/runtime.rs', 'reverie-kvm/tests/support/captured_write_signals.rs']", "paths=['reverie-kvm/src/elf.rs', 'reverie-kvm/src/executor.rs', 'reverie-kvm/src/process_signal_publication.rs']")
s=s.replace("scope='Scalar write low32 descriptor component; separate baseline/corrected source. No Hermit timer fix or parity claim'", "scope='Inactive process publication prerequisite; new private unit controls and unchanged existing VM consumers. No scheduler activation or Hermit timer repair claim'")
(q/'prepare.py').write_text(s)
(d/'caller-change.patch').write_text(''.join(difflib.unified_diff((old/'prepare.py').read_text().splitlines(True),s.splitlines(True),fromfile='before/prepare.py',tofile='after/prepare.py')))
for row in origins:row['copy']=rec(Path(row['copy']['path']))
(q/'RUNNER_ORIGINS.json').write_text(json.dumps({'predecessor':str(old),'files':origins,'delta':rec(d/'caller-change.patch'),'protocol_sources_unchanged':True,'scope':'prepare exact three paths/purpose and external selector data only'},indent=2)+'\n')
(q/'SETUP.json').write_text(json.dumps({'owner_slot':str(r),'source_root':str(r),'base':'000c15a1161ea2d58749431b5ddaaa97f7aa37d5','purpose':'Inactive process publication prerequisite, active serialization/default-path regression controls'},indent=2)+'\n')
v=json.loads((d/'prequalification-v1/SELECTORS.json').read_text());static=['parked_signals::parked_signal_actual_callback_frame_and_dequeue_contract','parked_signals::parked_signal_caught_posthook_mask_and_altstack_reach_real_frame','parked_signals::parked_signal_prepared_read_returns_eintr_without_restart','parked_signals::parked_signal_prepared_read_restarts_only_after_handler','parked_signals::parked_signal_prepared_partial_read_is_not_replayed','sibling_signal_delivery_native_plain_and_tool','leader_self_exec_lifetime_fork_0','leader_self_exec_lifetime_thread_0','signalfd_positioned_vector_flags_match_native_linux']
for i,n in enumerate(static,1):v['groups'][f'vm-{i:02}']={'artifact':'static','names':[n]}
v['unchanged_vm_declarations']=len(static);(q/'SELECTORS.json').write_text(json.dumps(v,indent=2)+'\n')
rows=[]
raw=subprocess.check_output(['git','ls-tree','-rz','HEAD'],cwd=r)
for entry in raw.split(b'\0'):
 if not entry:continue
 header,path=entry.split(b'\t',1);mode,typ,obj=header.decode().split();rel=path.decode();p=r/rel
 row={'path':rel,'mode':mode,'git_object':obj}
 if mode=='160000':row['sha256']=obj
 elif mode=='120000':row['sha256']=hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest()
 else:row['sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
 rows.append(row)
n='reverie-kvm/src/process_signal_publication.rs';rows.append({'path':n,'mode':'100644','sha256':hashlib.sha256((r/n).read_bytes()).hexdigest(),'new_product_source':True})
(q/'source-manifest.json').write_text(json.dumps(sorted(rows,key=lambda x:x['path']),indent=2)+'\n')
print('source entries',len(rows),'selectors',len(v['groups']),'caller delta',rec(d/'caller-change.patch'))
