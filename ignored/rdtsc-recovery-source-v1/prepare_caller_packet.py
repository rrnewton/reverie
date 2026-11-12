from pathlib import Path
import json,hashlib,difflib,stat,os
P=Path(__file__).resolve().parent;R=P.parents[1];Q=P/'qualification-v1';OLD=R/'ignored/publication-fd-stdin-cold-qualification-v1/qualification-v1';Q.mkdir(exist_ok=False);(Q/'observer').mkdir()
sha=lambda b:hashlib.sha256(b).hexdigest()
def rec(p):
 b=p.read_bytes();return {'path':str(p),'bytes':len(b),'mode':stat.S_IMODE(p.stat().st_mode),'sha256':sha(b)}
origins=[]
for name in ['prepare.py','phase.py','common.py','cache_lease.py','bind_dependencies.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json']:
 a=OLD/name;b=Q/name;b.write_bytes(a.read_bytes());origins.append({'original':rec(a),'copy_before_adaptation':rec(b)})
assert sha((Q/'observer/observer.py').read_bytes())=='137c9b42c3f9e081db59954e0ef692ed861f488be7c2bb8a15ef595c271b2179'
source_manifest=json.loads((P/'SOURCE-MANIFEST.json').read_text());(Q/'source-manifest.json').write_text(json.dumps([{'path':r['relative'],'mode':r['mode'],**({'sha256':r['sha256']} if 'sha256'in r else {'object':r['gitlink']})} for r in source_manifest],indent=2)+'\n')
changes=json.loads((P/'SOURCE-CONTINUITY.json').read_text())['changed_paths'];setup={'owner_slot':str(R),'source_root':str(P/'source'),'base':'79516661bf82d30ab2967c71834a6d47447b76ee','base_tree':'7620fe83f486d665d9d09d4f09f0e93636b862e4','source_files':[{'relative':x,'file':rec(P/'source'/x)} for x in changes],'lock':rec(P/'source/Cargo.lock'),'target':str(R/'target/rdtsc-recovery-v1-cold-v1'),'purpose':'CPL3 timestamp recovery only; public real-mode timestamp gap retained; no timer or full-backend claim'};(Q/'SETUP.json').write_text(json.dumps(setup,indent=2)+'\n')
controls=json.loads((P/'CONTROLS.json').read_text())['controls'];groups={};ids={'lib':'lib','static_elf':'static','vmcall':'vmcall','read_clock':'read-clock'}
for i,c in enumerate(controls,1):groups['timestamp-%02d'%i]={'artifact':ids[c['target']],'names':[c['selector']],'source':c['source'],'origin':c['origin'],'purpose':c['purpose']}
(Q/'SELECTORS.json').write_text(json.dumps({'groups':groups,'actual_inventory_pending':True,'exact_declarations':30},indent=2)+'\n')
(Q/'toolchain-standard-inputs.json').write_bytes((OLD/'toolchain-standard-inputs.json').read_bytes())
s=(Q/'prepare.py').read_text()
s=s.replace("('metadata','compile','list-lib','list-static','format','clippy','core-check')", "('metadata','compile','list-lib','list-static','list-vmcall','list-read-clock','format','clippy','core-check')")
s=s.replace("target/publication-fd-stdin-v3-cold-v1","target/rdtsc-recovery-v1-cold-v1")
s=s.replace("'--test','static_elf','--message-format=json'","'--test','static_elf','--test','vmcall','--test','read_clock','--message-format=json'")
s=s.replace("'--test','static_elf','--no-run'","'--test','static_elf','--test','vmcall','--test','read_clock','--no-run'")
s=s.replace("paths=['reverie-kvm/src/elf.rs', 'reverie-kvm/src/executor.rs', 'reverie-kvm/src/process_signal_publication.rs']",'paths='+repr(changes))
s=s.replace("dict(id='static',target='static_elf',kind=['test'])]","dict(id='static',target='static_elf',kind=['test']),dict(id='vmcall',target='vmcall',kind=['test']),dict(id='read-clock',target='read_clock',kind=['test'])]")
s=s.replace("HERE/'controls/compile/artifacts.json'","HERE/'retained-binaries/ARTIFACTS.json'")
needle="    cargo=str(TOOL/'cargo')\n";assert needle in s
s=s.replace(needle,"""    check_file(json_read(HERE/'SETUP.json')['lock'])
    inputs.append(file_record(HERE/'toolchain-standard-inputs.json'))
    for row in json_read(HERE/'toolchain-standard-inputs.json')['files']:
        check_file(row);inputs.append(row)
    admission=json_read(HERE/'TARGET-ADMISSION.json')
    inputs.append(file_record(HERE/'TARGET-ADMISSION.json'))
    require(admission['initially_empty'] is True and admission['mode']==0o700, 'fresh target admission missing')
    require(admission['path']==env['CARGO_TARGET_DIR'], 'wrong admitted target')
    cargo=str(TOOL/'cargo')
""",1)
needle="    plan=dict(path=str(HERE/(name+'-plan.json'))";assert needle in s
s=s.replace(needle,"    require([t.st_dev,t.st_ino,t.st_uid]==admission['identity'], 'fresh target identity changed')\n"+needle,1)
s=s.replace("scope='Inactive publisher and descriptor-entry composition; additive retirement/poison and unchanged unit/VM controls. No activation, FIFO or timer completion claim'","scope='CPL3 timestamp recovery: exact 30 declarations and existing public vmcall/clock neighbors; no public real-mode interception, timer completion or 75-cell parity claim from component phases'")
(Q/'prepare.py').write_text(s)
(P/'CALLER.patch').write_text(''.join(difflib.unified_diff((OLD/'prepare.py').read_text().splitlines(True),s.splitlines(True),fromfile='qualified/prepare.py',tofile='candidate/prepare.py')))
(Q/'RUNNER_ORIGINS.json').write_text(json.dumps({'sources':origins,'protocol_unchanged_helpers':['phase.py','common.py','cache_lease.py','bind_dependencies.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json'],'material_delta':str(P/'CALLER.patch'),'no_execution':True},indent=2)+'\n')
phases=['metadata','compile','format','list-lib','list-static','list-vmcall','list-read-clock',*groups,'core-check','clippy'];assert len(phases)==39
(P/'RUN-ORDER.json').write_text(json.dumps({'phases':phases,'metadata_followup':'bind_dependencies.py','compile_followup':'retain_artifacts.py','no_extra_retry_or_skipped_phase':True},indent=2)+'\n')
(P/'execute_phases.py').write_bytes((OLD.parent/'execute_phases.py').read_bytes())
print('prepared caller copies, source binding, exact selectors and material delta; no helper executed')
