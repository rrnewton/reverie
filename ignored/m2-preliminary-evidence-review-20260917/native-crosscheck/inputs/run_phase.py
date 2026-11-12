#!/usr/bin/python3
"""Run one fully bound phase through the unchanged observer and safehermit."""
import argparse
import os
from pathlib import Path
import sys
import time
from common import HERE, REPO, MIB, bounded_process, check_file, check_source, digest, file_record, json_read, owned, read, require, terminal, write_new
from prepare_phase import prepare
from readback import inspect


def cache_liveness(context, prefix, env):
    """Fresh census; old preparation records are not an ongoing cache lease."""
    query = bounded_process(['/bin/ps','-eo','pid=,ppid=,args='],REPO,env,prefix,5,4*MIB)
    require(query['returncode']==0 and not query['forced'], 'cache process census failed')
    cache = context['target_cache']['path']
    own = {os.getpid()}
    parent = os.getppid()
    for _ in range(64):
        if parent <= 1 or parent in own:
            break
        own.add(parent)
        raw = read('/proc/'+str(parent)+'/stat',65536).decode()
        parent = int(raw[raw.rindex(')')+2:].split()[1])
    users=[]
    excluded=[]
    for line in read(query['stdout']['path'],4*MIB).decode().splitlines():
        parts=line.strip().split(None,2)
        if len(parts)!=3:
            continue
        pid=int(parts[0])
        if pid in own:
            continue
        try:
            raw=read('/proc/'+str(pid)+'/stat',65536).decode()
            identity=raw[raw.rindex(')')+2:].split()
            cwd=os.readlink('/proc/'+str(pid)+'/cwd')
            environ=read('/proc/'+str(pid)+'/environ',MIB).split(b'\0')
        except (FileNotFoundError,ProcessLookupError):
            continue
        except PermissionError:
            # Three measured, protected operating-system processes cannot be
            # inspected through ptrace-gated cwd/environ. Exclude only their
            # complete bound identities, never their separately listed children.
            root=Path('/proc')/str(pid)
            require(root.stat().st_uid==os.getuid() or pid not in own, 'unexpected process ownership')
            if root.stat().st_uid!=os.getuid():
                continue
            binding=context['non_build_processes']
            require(read('/proc/sys/kernel/random/boot_id',128).decode().strip()==binding['boot_id'], 'non-build process binding is from another boot')
            raw=read(root/'stat',65536).decode();fields=raw[raw.rindex(')')+2:].split()
            actual=dict(pid=pid,parent=int(fields[1]),start_ticks=fields[19],
                        argv=parts[2],cgroup=read(root/'cgroup',4096).decode())
            require(actual in binding['processes'], 'unidentified same-user process could not be inspected: '+str(pid))
            excluded.append(actual)
            continue
        if ('CARGO_TARGET_DIR='+cache).encode() in environ or cwd==cache or cwd.startswith(cache+'/') or cache in parts[2]:
            users.append(dict(pid=pid,start_ticks=identity[19],cwd=cwd,args=parts[2]))
    write_new(Path(str(prefix)+'-excluded-system-processes.json'),excluded)
    write_new(Path(str(prefix)+'-users.json'),users)
    require(not users, 'another live process uses the assigned target cache')


def scm_readback(context, prefix, environment):
    actual={}
    for name,args in [('head',['rev-parse','HEAD']),('tree',['rev-parse','HEAD^{tree}']),
                      ('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
        query=bounded_process(['/usr/bin/git','-C',str(REPO),*args],REPO,environment,Path(str(prefix)+'-'+name),5,16*MIB)
        require(query['returncode']==0 and not query['forced'], 'source SCM readback failed')
        raw=read(query['stdout']['path'],16*MIB)
        actual[name]=__import__('hashlib').sha256(raw).hexdigest() if name=='index' else raw.decode().strip()
    write_new(Path(str(prefix)+'-binding.json'),actual)
    require(actual==context['scm'], 'head/tree/branch/index changed')


def emergency_observer_cleanup(observer, out, prefix, environment):
    """Use the bound observer's own authentication, never a bare stale PID/unit."""
    import importlib.util
    sys.path.insert(0,str(observer.parent))
    spec=importlib.util.spec_from_file_location('owned_observer_cleanup',observer)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    record=dict(attempted=True,authenticated=False)
    reference=None
    try:
        launch=json_read(out/'launch.json',MIB)
        fields=module.report_fields(out/'safehermit.report')
        # This exact report path is set by the frozen observer. An absent
        # report is a refusal, never a unit guess.
        unit=module.authenticate_report(fields,launch)
        reference=module.UnitReference('libsystemd.so.0',unit)
        deadline=time.monotonic()+5
        reference.method('RefUnit',deadline);reference.held=True
        values=module.unit_properties(reference,deadline,final=False)
        if values['ActiveState'] in ('inactive','failed') and values['MainPID']==0:
            require(values['ControlGroup']=='','terminal emergency scope is not empty')
            record.update(authenticated=True,already_terminal=True,unit=unit,properties=values)
        else:
            module.authenticate_service(values,unit,launch)
            record.update(authenticated=True,unit=unit,properties=values,kill_commands=[])
            module.kill_owned_unit(unit,record['kill_commands'])
            deadline=time.monotonic()+5
            while time.monotonic()<deadline:
                final=module.unit_properties(reference,deadline,final=True)
                if final['ActiveState'] in ('inactive','failed') and final['MainPID']==0 and final['ControlGroup']=='':
                    record['terminal_properties']=final
                    break
                time.sleep(0.05)
            require('terminal_properties' in record, 'emergency service cleanup did not reach inactive/empty')
    except BaseException as error:
        record['error']=repr(error)
    finally:
        if reference is not None:
            try: reference.close()
            except BaseException as error: record['close_error']=repr(error)
        write_new(prefix,record)
    # Emergency recovery is never qualification, even if scope was already dead.
    return record


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('plan');parser.add_argument('sha256')
    args=parser.parse_args()
    plan_path=owned(Path(args.plan));require(digest(plan_path)==args.sha256,'phase plan hash differs')
    plan=json_read(plan_path)
    check_file(plan['context']);context=json_read(plan['context']['path'])
    require(prepare(Path(plan['context']['path']),plan['group'],plan['phase_name'],Path(plan['output']))==plan,
            'actual plan differs from frozen template derivation')
    phase=plan['phase'];out=Path(plan['output']);environment=plan['environment']
    # Result/control files are bounded caller output outside observer.out. Runtime
    # payload output, fixtures, attempts and verification logs are inside it.
    control_root=owned(Path(context['control_root']))
    if not control_root.exists():
        control_root.mkdir(mode=0o700)
    owned(control_root,directory=True)
    control=owned(control_root/phase['name'])
    require(not control.exists(),'retain earlier phase controls/results')
    control.mkdir(mode=0o700)
    result=dict(schema=1,phase=phase['name'],plan=file_record(plan_path),accepted=False,
                terminal_authenticated=False,raw_status=None,source=context['source_manifest'])
    observer=HERE/'observer/observer.py'
    try:
        check_source(context)
        scm_readback(context,control/'scm-before',environment)
        cache_liveness(context,control/'cache-census',environment)
        check_file(context['observer'])
        require(context['observer']['path']==str(observer) and context['observer']['sha256']=='f10ab861f262dbbd18295d92e59e05174299b72b397f58de844ee1725266eae6', 'observer source differs')
        write_new(control/'before.json',dict(source=context['source_manifest'],inputs=context['inputs'],executables=context['executables']))
        argv=['/usr/bin/python3','-B',str(observer),'--out',str(out),
              '--cpu-usec',str(phase['aggregate_cpu_usec']),'--wall-seconds',str(phase['wall_seconds']),
              '--log-bytes',str(phase['lethal_stderr_bytes']),'/usr/bin/python3','-B',str(HERE/'admit.py'),str(plan_path),args.sha256]
        # The original observer owns phase wall+40. This outer transport gives
        # it another 10 seconds before requesting its same cleanup via SIGALRM,
        # then waits only 10+1 seconds. No shorter payload deadline is invented.
        outer=bounded_process(argv,REPO,environment,control/'observer',phase['wall_seconds']+50,MIB,observer=True)
        result['transport']=outer
        if outer['forced']:
            result['emergency_cleanup']=emergency_observer_cleanup(observer,out,control/'emergency-cleanup.json',environment)
        observed=json_read(out/'result.json',MIB)
        result['observer_result']=file_record(out/'result.json')
        result['raw_status']=observed.get('wrapper_exit_code')
        raw_status=terminal(observed,phase,outer,control/'service-post',environment,result)
        result['terminal_authenticated']=True
        payload=json_read(out/'payload-exit.json',MIB)
        result['payload_exit']=payload
        require(payload['reaped'] is True and payload['local_wait_timed_out'] is False, 'payload did not exit normally under observation')
        require(raw_status==(payload['returncode'] if payload['returncode']>=0 else 128-payload['returncode']), 'payload/service status boundary differs')
        check_source(context)
        result['readback']=inspect(plan,context,raw_status)
        result['accepted']=True
    except BaseException as error:
        result['error']=repr(error)
    finally:
        try:
            check_source(context)
            scm_readback(context,control/'scm-after',environment)
            result['final_source_inputs_unchanged']=True
        except BaseException as error:
            result['final_source_inputs_unchanged']=False;result['source_error']=repr(error);result['accepted']=False
        write_new(control/'result.json',result)
    print(str(control/'result.json'))
    return 0 if result['accepted'] else 1


if __name__=='__main__':
    raise SystemExit(main())
