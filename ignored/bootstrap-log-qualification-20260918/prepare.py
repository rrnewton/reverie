#!/usr/bin/python3
"""Freeze explicit component commands; preparation never invokes Cargo."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

from common import file_record, write_new

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
TOOL = Path('/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin')
NAMES = [
    'task::tests::command_bootstrap_arguments_preserve_types_and_raw_tail',
    'tracer::tests::command_bootstrap_ends_before_post_exec_and_later_guest_exec',
    'tracer::tests::spawn_fn_never_has_command_bootstrap_provenance',
    'tracer::tests::resolving_program_preserves_explicit_arg0',
    'task::tests::exec_generation_replaces_image_state_without_changing_old_holders',
    'tracer::tests::subscribed_restart_syscall_reaches_the_tool',
    'tracer::tests::unsubscribed_restart_syscall_retains_the_linux_result',
]


def scm():
    out = {}
    for name,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
        raw=subprocess.check_output(['/usr/bin/git','-C',str(REPO),*args],env=dict(os.environ,GIT_NO_LAZY_FETCH='1'),timeout=5)
        out[name]=hashlib.sha256(raw).hexdigest() if name=='index' else raw.decode().strip()
    return out


def main():
    name,kind=sys.argv[1:3]
    source=HERE/'source-manifest.json'
    if not source.exists():
        rows=[]
        raw=subprocess.check_output(['/usr/bin/git','-C',str(REPO),'ls-files','--stage','-z'],timeout=5)
        for item in raw.split(b'\0'):
            if not item:continue
            header,rel=item.split(b'\t',1);mode,blob,stage=header.decode().split();assert stage=='0'
            p=REPO/os.fsdecode(rel)
            row=dict(path=os.fsdecode(rel),mode=mode,git_blob=blob)
            if mode!='160000':
                b=os.fsencode(os.readlink(p)) if mode=='120000' else p.read_bytes()
                row.update(sha256=hashlib.sha256(b).hexdigest(),bytes=len(b))
            rows.append(row)
        write_new(source,rows)
    inputs=[];absent=[]
    for parent in [REPO,*REPO.parents,Path('/home/newton')]:
        for rel in ['.cargo/config','.cargo/config.toml']:
            p=parent/rel
            if p.is_file():inputs.append(file_record(p.resolve()))
            else:absent.append(str(p))
    for p in [REPO/'Cargo.lock',*map(lambda n:TOOL/n,['cargo','rustc','rustdoc']),Path('/usr/bin/python3').resolve(),Path('/usr/bin/git'),Path('/usr/bin/cc').resolve(),Path('/usr/bin/ld').resolve()]:
        inputs.append(file_record(p.resolve()))
    for p in [HERE/'phase.py',HERE/'prepare.py',HERE/'common.py',HERE/'cache_lease.py',*sorted((HERE/'observer').glob('*.py')),HERE/'observer/source-inputs.json']:
        inputs.append(file_record(p))
    inputs=list({r['path']:r for r in inputs}.values());absent=sorted(set(absent))
    target=REPO/'target/bootstrap-log';s=target.stat();lease=HERE/'lane.lease';l=lease.stat()
    env={
        'HOME':'/home/newton','PATH':'/usr/local/bin:/usr/bin:/bin','CARGO_HOME':'/home/newton/.cargo',
        'RUSTUP_HOME':'/home/newton/.rustup','CARGO_TARGET_DIR':str(target),'CARGO_BUILD_JOBS':'2',
        'THIRD_PARTY_BUILD_JOBS':'2','CARGO_NET_OFFLINE':'true','CARGO_NET_GIT_FETCH_WITH_CLI':'true',
        'RUSTFLAGS':'','LC_ALL':'C','LANG':'C','TZ':'UTC','CARGO_TERM_COLOR':'never',
        'TMPDIR':str(HERE/'tmp'),'XDG_CACHE_HOME':str(HERE/'cache'),'GIT_OPTIONAL_LOCKS':'0','GIT_NO_LAZY_FETCH':'1',
        'USER':'newton','LOGNAME':'newton','XDG_RUNTIME_DIR':'/run/user/212630',
        'DBUS_SESSION_BUS_ADDRESS':'unix:path=/run/user/212630/bus','RUSTC':str(TOOL/'rustc'),'RUSTDOC':str(TOOL/'rustdoc'),
    }
    cargo=str(TOOL/'cargo');selected=[]
    if kind=='metadata':argv=[cargo,'metadata','--offline','--locked','--format-version','1']
    elif kind=='compile':argv=[cargo,'test','--offline','--locked','-p','reverie-ptrace','--lib','--no-run','--message-format=json']
    elif kind=='check':argv=[cargo,'check','--offline','--locked','-p','reverie-core','--all-targets','--message-format=json']
    else:
        artifact=json.loads((HERE/'controls/compile/artifact.json').read_text())['file'];inputs.append(artifact)
        if kind=='list':argv=[artifact['path'],'--list','-Z','unstable-options','--format=json'];selected=NAMES
        else:
            assert kind=='test';selected=[NAMES[int(sys.argv[3])]]
            argv=[artifact['path'],*selected,'--exact','--test-threads=1','--nocapture','-Z','unstable-options','--format=json']
    large=kind in ('metadata','compile','check')
    plan=dict(path=str(HERE/(name+'-plan.json')),name=name,kind=kind,argv=argv,selected=selected,
        environment=env,scm=scm(),source_manifest=file_record(source),inputs=inputs,absent_inputs=absent,
        target=dict(path=str(target),identity=[s.st_dev,s.st_ino,s.st_uid]),
        lease=dict(path=str(lease),owner_slot=str(REPO),identity=[l.st_dev,l.st_ino,l.st_uid],state_directory=str(HERE/'lease-state')),
        output=str(HERE/'observer'/name),control=str(HERE/'controls'/name),
        limits=dict(aggregate_cpu_usec=600000000 if large else 30000000,wall_seconds=900 if large else 60,lethal_stderr_bytes=16777216 if large else 1048576),
        scope='Reverie component evidence only, not Hermit VALIDATE or replay parity')
    if (HERE/'dependency-inputs.json').exists():plan['dependencies']=file_record(HERE/'dependency-inputs.json')
    write_new(Path(plan['path']),plan)
    print(plan['path'],hashlib.sha256(Path(plan['path']).read_bytes()).hexdigest())


if __name__=='__main__':main()
