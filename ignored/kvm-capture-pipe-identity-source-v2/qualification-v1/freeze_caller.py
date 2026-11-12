#!/usr/bin/python3
"""Freeze preparation only: hash exact inputs; never create target, lease or execute product."""
import ast
import difflib
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
from common import check_file, digest, file_record, json_read, require, write_new

HERE=Path(__file__).resolve().parent
D=HERE.parent
V=HERE.parents[1]/'rdtsc-recovery-source-v6/qualification-v1'

# Update only still-unfrozen preparation summaries after complete explicit checks.
old=['admit_target.py','bind_dependencies.py','cache_lease.py','common.py','phase.py','prepare.py','retain_artifacts.py','observer/observer.py','observer/before_exec.py','observer/unit_reference.py','observer/source-inputs.json','toolchain-standard-inputs.json']
origins=[];delta=[]
for rel in old:
    before,after=V/rel,HERE/rel
    a,b=file_record(before),file_record(after)
    origins.append(dict(before=a,after=b,byte_identical=a['sha256']==b['sha256']))
    if a['sha256']!=b['sha256']:
        delta.extend(difflib.unified_diff(before.read_text().splitlines(True),after.read_text().splitlines(True),fromfile='V6/'+rel,tofile='capture/'+rel))
require([Path(r['after']['path']).name for r in origins if not r['byte_identical']]==['admit_target.py','bind_dependencies.py','prepare.py'],'unexpected maintained helper delta')
(HERE/'RUNNER_ORIGINS.json').write_text(json.dumps(dict(records=origins,new_helpers=[file_record(HERE/'execute.py'),file_record(HERE/'freeze_caller.py')],meaning='Only three maintained helper deltas; all phase/common/lease/observer/retention bytes unchanged.'),indent=2)+'\n')
(HERE/'CALLER-DELTA.patch').write_text(''.join(delta))
parsed=[]
for path in [*sorted(HERE.glob('*.py')),*sorted((HERE/'observer').glob('*.py'))]:
    ast.parse(path.read_text(),filename=str(path));parsed.append(file_record(path))
(HERE/'PREPARATION-CHECKS.json').write_text(json.dumps(dict(python_ast_only=parsed,no_product_compilation_or_test=True,freeze_python=dict(executable=sys.executable,argv=sys.argv,optimize=sys.flags.optimize,debug=__debug__),read_limits=['Historical V1 duplicate delete/add apply_patch was refused; its complete history is preserved.','Historical V1 missing lock and first compiler failure remain frozen; V2 explicitly prepares the same lock carrier before execution.','Historical V1 static-freezer symlink schema refusal is preserved; this successor keeps the corrected literal-alias and resolved-byte checks.','No product/helper execution beyond this static preparation freezer.']),indent=2)+'\n')

files={};links={};gitlinks={}
def add(path, expected=None):
    path=Path(path)
    if path.is_symlink():
        row=dict(path=str(path),target=os.readlink(path))
        if expected:
            require(row['target']==expected['target'],'alias changed')
        links[str(path)]=row
    else:
        row=file_record(path)
        if expected:
            require(row['sha256']==expected['sha256'] and row['bytes']==expected['bytes'] and row['mode']==expected.get('file_mode',expected.get('mode',row['mode'])),'input changed: '+str(path))
        files[str(path)]=row
    return row

# Authenticate the complete frozen source/evidence closure, without reading live build targets.
for row in json_read(D/'INPUTS.json')['records']:
    if row['kind']=='unexpanded_gitlink':
        p=Path(row['path']);require(p.is_dir() and not p.is_symlink(),'old gitlink absent');gitlinks[str(p)]=row
    else:add(row['path'],row)
for name in ['TARGET.json','INPUTS.json','READBACK.json','SOURCE.patch','SOURCE-MANIFEST.json','SELECTORS.json','REPORT.md','V1-TO-V2.patch','SOURCE-CONTINUITY.json']:
    add(D/name)
original=json_read(D/'SOURCE-MANIFEST.json')
carrier=json_read(HERE/'source-manifest.json')
require(len(original)==2626 and len(carrier)==2627,'wrong source denominator')
original_by_name={r['relative']:r for r in original}
require({r['path'] for r in carrier}==set(original_by_name)|{'Cargo.lock'},'carrier changed product entries')
for row in carrier:
    p=HERE/'source'/row['path']
    if row['mode']=='160000':
        require(p.is_dir() and not p.is_symlink(),'carrier gitlink absent');gitlinks[str(p)]=dict(path=str(p),kind='unexpanded_gitlink',mode='160000',sha256=row.get('sha256'))
        continue
    actual=add(p)
    actual_sha=hashlib.sha256(os.fsencode(actual['target'])).hexdigest() if row['mode']=='120000' else actual['sha256']
    require(actual_sha==row['sha256'],'carrier manifest differs')
    if row['path'] in original_by_name:
        before=original_by_name[row['path']]
        require(row['sha256']==before['sha256'] and row['mode']==before['mode'],'carrier product differs')
        if row['mode']!='120000':require(p.stat().st_ino!=Path(before['path']).stat().st_ino,'carrier aliases original inode')
lock=json_read(HERE/'SETUP.json')['lock'];check_file(lock)
require(lock['sha256']=='1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856','wrong V6 lock')
add(V/'source-manifest.json');add(V/'SETUP.json')
require(digest(Path(json_read(V/'SETUP.json')['source_root'])/'Cargo.lock')==lock['sha256'],'V6 lock no longer identical')
add(Path(json_read(V/'SETUP.json')['source_root'])/'Cargo.lock')
for row in json_read(HERE/'CALLER-CONTINUITY.json')['unchanged_runtime_helpers']:
    add(row['before']['path'],row['before'])
    add(row['after']['path'],row['after'])
for path in sorted(HERE.iterdir()):
    if path.is_file():add(path)
for path in sorted((HERE/'observer').iterdir()):
    require(path.is_file(),'unexpected observer runtime artifacts before execution');add(path)
for row in json_read(HERE/'observer/source-inputs.json')['inputs']:
    path=Path(row['path'])
    if path.is_symlink():add(path)
    actual=add(path.resolve(strict=True));require(actual['sha256']==row['sha256'] and actual['bytes']==row['bytes'],'observer input changed')
for row in json_read(HERE/'toolchain-standard-inputs.json')['files']:add(row['path'],row)
for row in origins:add(row['before']['path'])
predecessor=json_read(HERE/'PREDECESSOR-ELFS.json');add(predecessor['source']);require(digest(predecessor['source'])==predecessor['sha256'],'predecessor binding changed')
# Explicit current toolchain/program identities; loader closure is authenticated by reviewed queries before each future execution.
root=Path('/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin')
programs=[root/n for n in ['cargo','rustc','rustdoc','cargo-clippy','clippy-driver','rustfmt']]+[Path('/usr/bin')/n for n in ['python3','git','gcc','ld','timeout','ldd','readelf']]
for path in programs:
    for candidate in [path,*path.parents]:
        if candidate.is_symlink():add(candidate)
    add(path.resolve(strict=True))
for parent in [HERE/'source',*(HERE/'source').parents,Path('/home/newton')]:
    for rel in ['.cargo/config','.cargo/config.toml']:
        p=parent/rel
        if p.is_file():add(p.resolve())
absent=[str(HERE/name)for name in ['target','lane.lease','lease-state','tmp','cache','controls','loader','launch','TARGET-ADMISSION.json','retained-binaries','EXECUTION-RESULT.json']]
for name in absent:require(not Path(name).exists() and not Path(name).is_symlink(),'runtime destination already exists')
selection=json_read(HERE/'SELECTORS.json');require(len(selection['groups'])==len(selection['proposed_order'])==63,'selection count')
require(len({(g['artifact'],tuple(g['names']))for g in selection['groups'].values()})==63,'duplicate selected declaration')
require(len(json_read(HERE/'PHASE-PLAN.json')['phases'])==72,'phase count')
index=dict(files=sorted(files.values(),key=lambda r:r['path']),symlinks=sorted(links.values(),key=lambda r:r['path']),unexpanded_gitlinks=sorted(gitlinks.values(),key=lambda r:r['path']),must_be_absent_before_execution=absent,scope='Prepared exact source, caller, retained history and toolchain; no current runtime results or admission')
write_new(HERE/'CALLER-INPUTS.json',index)
target=dict(status='PREPARED ONLY; requires root inspection before execution',inputs=file_record(HERE/'CALLER-INPUTS.json'),source_target=file_record(D/'TARGET.json'),source_patch=file_record(D/'SOURCE.patch'),source_manifest=file_record(HERE/'source-manifest.json'),source_carrier=file_record(HERE/'CARRIER-CONTINUITY.json'),setup=file_record(HERE/'SETUP.json'),readme=file_record(HERE/'README.md'),execution=file_record(HERE/'EXECUTION.md'),driver=file_record(HERE/'execute.py'),phase_plan=file_record(HERE/'PHASE-PLAN.json'),helper_delta=file_record(HERE/'CALLER-DELTA.patch'),origins=file_record(HERE/'RUNNER_ORIGINS.json'),command=['/usr/bin/python3','-B',str(HERE/'execute.py'),'run'],environment={'PYTHONOPTIMIZE':'0'},cwd=str(HERE),planned_phases=72,planned_tests=63,executed_phases=0)
write_new(HERE/'CALLER-TARGET.json',target)
for row in index['files']:check_file(row)
for row in index['symlinks']:require(Path(row['path']).is_symlink() and os.readlink(row['path'])==row['target'],'final alias drift')
write_new(HERE/'CALLER-READBACK.json',dict(target=file_record(HERE/'CALLER-TARGET.json'),inputs=file_record(HERE/'CALLER-INPUTS.json'),regular_files=len(files),symlinks=len(links),unexpanded_gitlinks=len(gitlinks),all_current_bytes_modes_unchanged=True,carrier_original_entries_unchanged=2626,only_added_lock=lock,python_ast_helpers=len(parsed),planned_phases=72,planned_tests=63,runtime_executed=False,admission_or_lease_created=False))
print(json.dumps(dict(target=file_record(HERE/'CALLER-TARGET.json'),inputs=file_record(HERE/'CALLER-INPUTS.json'),readback=file_record(HERE/'CALLER-READBACK.json'),files=len(files),symlinks=len(links),gitlinks=len(gitlinks)),indent=2))
