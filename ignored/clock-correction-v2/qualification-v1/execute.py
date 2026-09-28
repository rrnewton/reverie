#!/usr/bin/python3
"""Execute the reviewed finite sequence once; stop on the first failed phase or guard."""
import os,stat,subprocess,sys,time
from pathlib import Path
from common import check_file,digest,file_record,json_read,read,require,write_new
HERE=Path(__file__).resolve().parent

def authenticate():
 target=json_read(HERE/'CALLER-TARGET.json');check_file(target['inputs']);index=json_read(target['inputs']['path'])
 for row in index['files']:check_file(row)
 for row in index['symlinks']:
  p=Path(row['path']);require(p.is_symlink() and os.readlink(p)==row['target'],'input alias changed')
 for name in index['must_be_absent_before_execution']:
  p=Path(name);require(not p.exists() and not p.is_symlink(),'run destination already exists: '+name)
 plan=json_read(HERE/'metadata-plan.json');p=Path(plan['target']['path']);s=p.lstat()
 require(stat.S_ISDIR(s.st_mode) and stat.S_IMODE(s.st_mode)==0o700 and [s.st_dev,s.st_ino,s.st_uid]==plan['target']['identity'] and not list(p.iterdir()),'fresh target differs')
 lease=Path(plan['lease']['path']);s=lease.lstat()
 require(stat.S_ISREG(s.st_mode) and stat.S_IMODE(s.st_mode)==0o600 and s.st_size==0 and [s.st_dev,s.st_ino,s.st_uid]==plan['lease']['identity'],'lease not pristine')
 state=Path(plan['lease']['state_directory']);require(state.is_dir() and not state.is_symlink() and not list(state.iterdir()),'private state not pristine')
 phases=json_read(HERE/'PHASES.json')['phases'];selectors=json_read(HERE/'SELECTORS.json')
 tests=[p['selected'][0] for p in phases if p['kind']=='test']
 require(tests==selectors['ordered_names'] and len(tests)==selectors['exact_declarations'] and len(tests)==len(set(tests)),'finite test selection changed')
 require(any(line.endswith('[vsyscall]') for line in Path('/proc/self/maps').read_text().splitlines()),'legacy-vsyscall mapping absent; existing early return cannot qualify')
 return phases

def command(label,argv):
 prefix=HERE/'launch'/label;start=time.monotonic()
 with prefix.with_suffix('.stdout').open('xb') as out,prefix.with_suffix('.stderr').open('xb') as err:
  p=subprocess.run(argv,cwd=HERE,env={**os.environ,'PYTHONOPTIMIZE':'0','PYTHONDONTWRITEBYTECODE':'1'},stdin=subprocess.DEVNULL,stdout=out,stderr=err,check=False)
 record=dict(argv=argv,returncode=p.returncode,elapsed_seconds=time.monotonic()-start,stdout=file_record(prefix.with_suffix('.stdout')),stderr=file_record(prefix.with_suffix('.stderr')))
 write_new(prefix.with_suffix('.json'),record);return record

def main():
 require(sys.flags.optimize==0 and os.environ.get('PYTHONOPTIMIZE')=='0','explicit unoptimized Python required')
 require(sys.argv[1:]==['run'],'execute.py run only after root authorization')
 phases=authenticate();journal=dict(sequence=[p['name'] for p in phases],planned_phases=len(phases),planned_tests=sum(p['kind']=='test' for p in phases),attempts=[],accepted=[],error=None,no_retry=True,prior_lease_untouched=True)
 try:
  for row in phases:
   name=row['name'];prepared=command('prepare-'+name,['/usr/bin/python3','-B',str(HERE/'prepare.py'),name]);require(prepared['returncode']==0,'phase preparation refused: '+name)
   plan=HERE/(name+'-plan.json');outcome=command('phase-'+name,['/usr/bin/python3','-B',str(HERE/'phase.py'),'launch',str(plan),digest(plan)])
   attempt=dict(name=name,transport=outcome);journal['attempts'].append(attempt);receipt=HERE/'controls'/name/'result.json'
   if receipt.is_file():attempt['receipt']=file_record(receipt)
   require(outcome['returncode']==0,'phase failed/refused; no retry: '+name)
   result=json_read(receipt);require(result['accepted'] and result['terminal_authenticated'] and result['inputs_unchanged'] and result['raw_status']==0,'phase did not qualify: '+name)
   completion=HERE/'lease-state'/(digest(plan)+'.json');completed=json_read(completion)
   require(completed['plan_sha256']==digest(plan) and completed['result']==file_record(receipt),'normal phase completion missing')
   if row['kind']=='test':
    raw=read(HERE/'observer'/name/'stderr',16*1024**2).decode()
    require('skipping legacy-vsyscall test' not in raw,'legacy control skipped its real path')
    for marker in row.get('required_markers',[]):require(raw.splitlines().count(marker)==1,'missing/duplicate exact child completion marker: '+marker)
   journal['accepted'].append(name)
   if name=='metadata':
    bound=command('bind-dependencies',['/usr/bin/python3','-B',str(HERE/'bind_dependencies.py')]);require(bound['returncode']==0,'metadata closure binding refused')
   if name=='compile':
    retained=command('retain-artifacts',['/usr/bin/python3','-B',str(HERE/'retain_artifacts.py')]);require(retained['returncode']==0,'artifact retention refused')
 except BaseException as error:
  journal['error']=repr(error);raise
 finally:write_new(HERE/'EXECUTION-RESULT.json',journal)
if __name__=='__main__':main()
