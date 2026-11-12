#!/usr/bin/python3
"""Run exactly the retained native population on an authenticated built ELF."""
from pathlib import Path
import hashlib,json,os,re,runpy,subprocess
HERE=Path(__file__).resolve().parent
PLAN_SHA256='9b3ecdc839a0f978e2fdf8c20bfdff1046a173306cc0e66a69b28b9ec6d98aff'

def main():
    raw=(HERE/'plan.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest()==PLAN_SHA256
    plan=json.loads(raw)
    helper=Path(plan['helpers']['path'])
    assert hashlib.sha256(helper.read_bytes()).hexdigest()==plan['helpers']['sha256']
    functions=runpy.run_path(str(helper),run_name='reviewed_helpers_only')
    require,digest,read_bounded,write_new,check_executable=(functions[k] for k in ['require','digest','read_bounded','write_new','check_executable'])
    require(plan['execution']==['/usr/bin/python3','-B',str(HERE/'launch.py')],'wrong caller')
    require([s['name'] for s in plan['stages']]==['native'],'unexpected stage')
    selected=plan['selected_tests'];require(plan['required_count']==44 and len(selected)==44 and len(set(selected))==44,'changed native population')
    prior=json.loads(read_bounded(Path(plan['source_root'])/'ignored/prejoin-failure-implementation-20260917/cargo-v16/plan.json',1024**2))
    require(selected==prior['selected_tests'],'original native identities changed')
    inventory=json.loads(read_bounded(plan['inventory']['path'],1024**2))
    require(len(inventory['names'])==453 and inventory['count']==453 and len(set(inventory['names']))==453,'inventory changed')
    require(all(inventory['names'].count(name)==1 for name in selected),'missing selected native')
    stage=plan['stages'][0];artifact=plan['artifact']
    require(stage['payload']==[artifact['path'],'--exact',*selected,'--test-threads=1','--nocapture'],'changed native arguments')
    require(stage['argv'][stage['argv'].index('--log-bytes')+2:]==stage['payload'],'payload/argv mismatch')
    root=Path(plan['run_root'])
    for path in [root,Path(plan['observer_root'])]:require(not path.exists() and not path.is_symlink(),'retain prior output: '+str(path))
    functions['check_inputs'](plan);check_executable(artifact)
    root.mkdir(mode=0o700);Path(plan['tmpdir']).mkdir(mode=0o700)
    environment={key:os.environ[key] for key in plan['environment_keys'] if key in os.environ};environment.update(plan['environment_fixed'])
    write_new(root/'launch.json',dict(plan_sha256=PLAN_SHA256,caller_sha256=digest(__file__),execution=plan['execution'],cwd=str(Path.cwd()),environment=environment,source_binding=plan['source_binding'],artifact=artifact,selected=selected,scope=plan['scope']))
    try:
        functions['check_inputs'](plan);check_executable(artifact)
        write_new(root/'native-dispatch.json',dict(argv=stage['argv'],cwd=stage['cwd'],artifact=artifact,selected=selected))
        with (root/'native-observer.stdout').open('xb') as stdout,(root/'native-observer.stderr').open('xb') as stderr:
            process=subprocess.run(stage['argv'],cwd=stage['cwd'],env=environment,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr)
        result_path=Path(stage['out'])/'result.json';result=json.loads(read_bounded(result_path,1024**2))
        write_new(root/'native-readback.json',dict(observer_exit=process.returncode,result_path=str(result_path),result_sha256=digest(result_path),result=result))
        functions['require_terminal'](result,process.returncode,stage,root,environment)
        raw_stdout=read_bounded(Path(stage['out'])/'stdout',stage['reader_limit_bytes']);raw_stderr=read_bounded(Path(stage['out'])/'stderr',stage['reader_limit_bytes']);text=raw_stdout.decode()
        summaries=re.findall(r'^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;',text,re.M)
        require(summaries==[('44','0','0','0','409')],'wrong native summary')
        passed=re.findall(r'^test (.+) \.\.\. ok$',text,re.M)
        require(sorted(passed)==sorted(selected),'wrong passing native identities')
        functions['check_inputs'](plan);check_executable(artifact)
        record=dict(status='passed',selected=selected,summary_counts=summaries[0],stdout_sha256=hashlib.sha256(raw_stdout).hexdigest(),stderr_sha256=hashlib.sha256(raw_stderr).hexdigest(),cpu_nsec=result['final_accounting']['cpu_usage_nsec'],wall_seconds=result['elapsed_seconds'],scope=plan['scope'])
        write_new(root/'summary.json',record);print(json.dumps(record))
    except Exception as error:
        write_new(root/'summary.json',dict(status='failed',error=str(error),scope=plan['scope']))
        raise

if __name__=='__main__':main()
