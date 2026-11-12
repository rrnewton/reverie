#!/usr/bin/python3
"""Append fresh read-only lifecycle proof. Never complete or rewrite the lease."""
import fcntl,hashlib,json,os,sys,time
from pathlib import Path
F=Path(__file__).resolve().parent; Q=F.parent
sys.path.insert(0,str(Q))
from common import bounded_process,check_file,digest,file_record,json_read,read,require,write_new
import cache_lease,phase
require(sys.flags.optimize==0,'optimization must be disabled')
plan=json_read(Q/'neighbor-05-plan.json');sha=digest(Q/'neighbor-05-plan.json')
r=json_read(Q/'controls/neighbor-05/result.json');o=json_read(Q/'observer/neighbor-05/result.json')
require(r['plan_sha256']==sha and r['raw_status']==137 and r['accepted'] is False and r['terminal_authenticated'] is False,'wrong refusal')
require(o['observer_error'].startswith('TimeoutError:') and [x for x in o['systemd_calls'] if x.get('function')=='sd_bus_get_property' and x.get('property')=='Id' and x.get('status')==-110] and not [x for x in o['systemd_calls'] if x.get('function')=='sd_bus_get_property' and x.get('property')!='Id'],'wrong startup failure')
require('authenticated_service' not in o and 'payload_authorization' not in o and o['final_accounting'] is None,'unexpected authorization or accounting')
absent=[Q/'observer/neighbor-05'/n for n in ['before-exec-receipt.json','payload.json','payload-exit.json']]
for p in absent:require(not p.exists() and not p.is_symlink(),'unexpected execution evidence')
require((Q/'observer/neighbor-05/stdout').stat().st_size==(Q/'observer/neighbor-05/stderr').stat().st_size==0,'unexpected raw output')
fd=cache_lease.open_bound(plan['lease'])
try:
 fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);cache_lease.check_held(fd,plan['lease'],sha)
 token=os.pread(fd,4096,0);completion=Q/'lease-state'/(sha+'.json');require(not completion.exists(),'unexpected completion')
 unit=o['kill_commands'][0]['argv'][-1]
 properties='Id,LoadState,ActiveState,SubState,MainPID,ControlGroup,ExecMainPID,ExecMainStartTimestampMonotonic,ExecMainExitTimestampMonotonic,ExecMainCode,ExecMainStatus,Result,CPUUsageNSec,TasksCurrent'
 query=bounded_process(['/usr/bin/systemctl','--user','show',unit,'--property='+properties],Q,plan['environment'],F/'terminal-unit',5,65536)
 require(not query['forced'] and query['returncode']==0,'fresh unit query refused')
 for key in ['stdout','stderr']:require(query[key]['bytes']<=65536,'unit output bound exceeded');check_file(query[key])
 raw=read(query['stdout']['path'],65537);require(len(raw)==query['stdout']['bytes'],'unit output changed')
 vals=dict(line.split('=',1)for line in raw.decode().splitlines()if '=' in line)
 require(vals.get('Id')==unit and vals.get('ActiveState') in ['inactive','failed'] and vals.get('MainPID')=='0' and vals.get('ControlGroup')=='','service not terminal/empty')
 wrapper=o['launch']['wrapper_identity'];spec=json_read(Q/'observer/neighbor-05/before-exec.json')
 ids={'wrapper':wrapper['pid'],'observer':spec['observer_pid'],'service_main':2074316}
 observations={}
 for name,pid in ids.items():
  p=Path('/proc')/str(pid)/'stat'
  try:
   raw=p.read_text();fields=raw[raw.rindex(')')+2:].split();observations[name]=dict(pid=pid,stat=raw,start_ticks=int(fields[19]),state=fields[0])
  except FileNotFoundError:observations[name]=dict(pid=pid,absent=True)
 require(all(x.get('absent') for x in observations.values()),'recorded process still exists; investigate identity')
 members=[];errors=[];transient=0
 for p in Path('/proc').iterdir():
  if not p.name.isdecimal():continue
  try:
   raw=(p/'stat').read_text();fields=raw[raw.rindex(')')+2:].split()
   if int(fields[3])==wrapper['pid']:members.append(dict(pid=int(p.name),stat=raw))
  except FileNotFoundError:transient+=1
  except OSError as e:errors.append(dict(path=str(p/'stat'),error=repr(e)))
 require(not members and not errors,'wrapper session proof incomplete')
 phase.check(plan);phase.scm(plan,F/'SCM-FINAL.json');cache_lease.check_held(fd,plan['lease'],sha)
 proof=dict(plan=file_record(Q/'neighbor-05-plan.json'),refused_result=file_record(Q/'controls/neighbor-05/result.json'),original_observer=file_record(Q/'observer/neighbor-05/result.json'),query=query,properties=vals,recorded_processes=observations,wrapper_session=dict(sid=wrapper['pid'],members=members,read_errors=errors,transient_disappearances=transient),gate_sources=[file_record(Q/'observer'/n)for n in ['observer.py','before_exec.py','unit_reference.py']],gate_analysis='The first Unit.Id property read threw before authenticate_service/acquire_before_exec and the sole authorization send. The bound child requires authenticated exec response before writing its receipt and fd-exec. No authorization path was reached; absent receipt/payload records corroborate rather than establish this alone.',absent_execution_records=[str(p) for p in absent],cpu_accounting_available=vals.get('CPUUsageNSec') not in [None,'infinity','18446744073709551615'],available_cpu_nsec=vals.get('CPUUsageNSec'),original_missing_accounting_preserved=True,lease=dict(binding=plan['lease'],exclusive_nonblocking_acquired=True,token=token.decode(),token_sha256=hashlib.sha256(token).hexdigest(),completion_absent=True,no_token_or_completion_mutation=True,continuation_forbidden_on_this_lease=True))
finally:os.close(fd)
proof['lease']['exclusive_hold_closed']=True
write_new(F/'TERMINAL-LIFECYCLE.json',proof)
print(json.dumps(file_record(F/'TERMINAL-LIFECYCLE.json'),indent=2))
