#!/usr/bin/python3
"""Bounded Reverie component phases using the unchanged reviewed observer."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time

import cache_lease
from common import bounded_process, check_file, digest, file_record, json_read, read, require, terminal, write_new

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]


def check(plan):
    check_file(plan['source_manifest'])
    for row in json_read(plan['source_manifest']['path']):
        p = REPO / row['path']
        if row['mode'] == '160000':
            continue  # Unused gitlink pin, not a claim of expanded source.
        if row['mode'] == '120000':
            require(p.is_symlink() and hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest() == row['sha256'], 'source symlink changed')
        else:
            require(p.is_file() and not p.is_symlink(), 'source type changed')
            require(bool(p.stat().st_mode & 0o111) == (row['mode'] == '100755'), 'source mode changed')
            require(digest(p) == row['sha256'], 'source changed: ' + str(p))
    for row in plan['inputs']:
        check_file(row)
    for name in plan['absent_inputs']:
        require(not Path(name).exists() and not Path(name).is_symlink(), 'new Cargo configuration: ' + name)
    p = Path(plan['target']['path'])
    s = p.stat()
    require([s.st_dev,s.st_ino,s.st_uid] == plan['target']['identity'] and not p.is_symlink(), 'owned target changed')
    if plan.get('dependencies'):
        check_file(plan['dependencies'])
        for item in json_read(plan['dependencies']['path']):
            check_file(item)


def scm(plan, prefix):
    state = {}
    for name, args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
        p = subprocess.run(['/usr/bin/git','-C',str(REPO),*args],env=plan['environment'],capture_output=True,timeout=5,check=True)
        state[name] = hashlib.sha256(p.stdout).hexdigest() if name == 'index' else p.stdout.decode().strip()
    write_new(prefix,state)
    require(state == plan['scm'],'SCM state changed')


def payload(plan, sha):
    fd = cache_lease.join(plan['lease'], sha)
    try:
        check(plan)
        for k,v in plan['environment'].items():
            require(os.environ.get(k) == v, 'payload environment changed: '+k)
        extras = [k for k in os.environ if k not in plan['environment'] and (k.startswith(('CARGO_','RUST','LD_','NEXTEST_','REVERIE_','THIRD_PARTY_')) or k in ('CC','CXX','CFLAGS','CPPFLAGS','CXXFLAGS','LDFLAGS'))]
        require(not extras, 'undeclared overrides: '+str(extras))
        group = read('/proc/self/cgroup',4096).decode()
        require('/safehermit-' in group and group.strip().endswith('.service'),'payload is not in observed service')
        out = Path(plan['output'])
        write_new(out/'payload.json',dict(argv=plan['argv'],cwd=str(REPO),environment=dict(os.environ),cgroup=group,pid=os.getpid(),plan_sha256=sha))
        started = time.monotonic()
        child = subprocess.Popen(plan['argv'],cwd=REPO,stdin=subprocess.DEVNULL)
        code = child.wait()  # Actual observer owns complete aggregate lifetime.
        write_new(out/'payload-exit.json',dict(returncode=code,reaped=True,elapsed_seconds=time.monotonic()-started))
        check(plan)
        cache_lease.check_held(fd,plan['lease'],sha)
        return code if code >= 0 else 128-code
    finally:
        os.close(fd)


def inspect(plan, raw):
    kind = plan['kind']
    if kind == 'metadata':
        value = json.loads(raw)
        require(value['workspace_root'] == str(REPO),'metadata workspace changed')
        return dict(package_count=len(value['packages']),workspace_root=value['workspace_root'])
    if kind in ('compile','check'):
        rows = [json.loads(x) for x in raw.splitlines()]
        require([r['success'] for r in rows if r.get('reason')=='build-finished'] == [True], 'missing successful Cargo completion')
        diagnostics = [r for r in rows if r.get('reason') == 'compiler-message']
        write_new(Path(plan['control'])/'compiler-diagnostics.json',diagnostics)
        require(not diagnostics,'structured compiler diagnostics require disposition')
        if kind == 'check':
            return dict(cargo_complete=True,compiler_diagnostics=0)
        matches = [r for r in rows if r.get('reason')=='compiler-artifact' and r.get('manifest_path')==str(REPO/'reverie-ptrace/Cargo.toml') and r.get('target',{}).get('name')=='reverie_ptrace' and r.get('target',{}).get('kind')==['lib'] and r.get('profile',{}).get('test') is True and r.get('executable')]
        require(len(matches)==1,'Cargo must emit exactly one selected test executable')
        path = Path(matches[0]['executable'])
        require(path.resolve().is_relative_to(Path(plan['target']['path'])) and not path.is_symlink() and os.access(path,os.X_OK),'test artifact escaped owned cache')
        result = dict(cargo_artifact=matches[0],file=file_record(path))
        write_new(Path(plan['control'])/'artifact.json',result)
        return result
    rows = [json.loads(x) for x in raw.splitlines()]
    if kind == 'list':
        names = [r['name'] for r in rows if r.get('type')=='test']
        require(len(names)==len(set(names)),'duplicate test names')
        require(all(names.count(x)==1 for x in plan['selected']),'missing selected test')
        return dict(names=names,count=len(names),selected=plan['selected'])
    require(kind=='test','unknown result type')
    started = [r['name'] for r in rows if r.get('type')=='test' and r.get('event')=='started']
    ended = [r for r in rows if r.get('type')=='test' and r.get('event') in ('ok','failed','ignored')]
    suites = [r for r in rows if r.get('type')=='suite' and r.get('event') in ('ok','failed')]
    write_new(Path(plan['control'])/'outcomes.json',dict(started=started,ended=ended,suites=suites))
    require(started == plan['selected'] and len(ended)==len(plan['selected']) and [r['name'] for r in ended]==plan['selected'] and all(r['event']=='ok' for r in ended),'actual selected outcomes differ')
    require(len(suites)==1 and suites[0]['event']=='ok' and suites[0]['passed']==len(plan['selected']) and suites[0]['failed']==suites[0]['ignored']==suites[0]['measured']==0,'test count/failure/skip mismatch')
    return dict(executed=len(ended),summary=suites[0],names=started)


def launch(plan, sha):
    fd = cache_lease.claim(plan['lease'],sha)
    control = Path(plan['control']);control.mkdir(mode=0o700)
    result = dict(plan_sha256=sha,accepted=False,observer_attempted=False,terminal_authenticated=False,raw_status=None)
    try:
        check(plan);scm(plan,control/'scm-before.json')
        phase=plan['limits']
        argv=['/usr/bin/python3','-B',str(HERE/'observer/observer.py'),'--out',plan['output'],'--cpu-usec',str(phase['aggregate_cpu_usec']),'--wall-seconds',str(phase['wall_seconds']),'--log-bytes',str(phase['lethal_stderr_bytes']),'/usr/bin/python3','-B',str(HERE/'phase.py'),'payload',plan['path'],sha]
        write_new(control/'dispatch.json',dict(argv=argv,environment=plan['environment'],cwd=str(REPO)))
        result['observer_attempted']=True
        outer=bounded_process(argv,REPO,plan['environment'],control/'observer',phase['wall_seconds']+50,1024**2,observer=True)
        result['transport']=outer
        observed=json_read(Path(plan['output'])/'result.json')
        result['observer_result']=file_record(Path(plan['output'])/'result.json')
        result['raw_status']=observed.get('wrapper_exit_code')
        raw=read(Path(plan['output'])/'stdout',16*1024**2)
        read(Path(plan['output'])/'stderr',16*1024**2)
        # Preserve raw compiler/test failures before requiring successful exit.
        try: result['readback']=inspect(plan,raw)
        except Exception as e: result['readback_error']=repr(e)
        code=terminal(observed,phase,outer,control/'service-post',plan['environment'],result)
        receipt=json_read(Path(plan['output'])/'payload-exit.json')
        result['payload_exit']=receipt
        require(receipt['reaped'] and code==(receipt['returncode'] if receipt['returncode']>=0 else 128-receipt['returncode']),'payload status differs')
        require(code==0 and outer['returncode']==0,'actual payload failed')
        require('readback_error' not in result,'structured readback failed')
        result['accepted']=True
    except BaseException as e:
        result['error']=repr(e)
    finally:
        try:
            check(plan);scm(plan,control/'scm-after.json');cache_lease.check_held(fd,plan['lease'],sha)
            result['inputs_unchanged']=True
        except BaseException as e:
            result['inputs_unchanged']=False;result['input_error']=repr(e);result['accepted']=False
        write_new(control/'result.json',result)
        cache_lease.complete(fd,plan['lease'],sha,control/'result.json');os.close(fd)
    print(json.dumps(result),flush=True)
    return 0 if result['accepted'] else 1


if __name__=='__main__':
    mode,path,sha=sys.argv[1:]
    require(digest(path)==sha,'phase plan changed')
    plan=json_read(path);require(plan['path']==path,'wrong plan path')
    raise SystemExit(payload(plan,sha) if mode=='payload' else launch(plan,sha))
