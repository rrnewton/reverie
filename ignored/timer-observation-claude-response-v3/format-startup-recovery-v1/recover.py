from pathlib import Path
import fcntl, hashlib, os, sys
D=Path(__file__).resolve().parent; Q=D.parent/'positive-v2';sys.path.insert(0,str(Q))
from common import check_file,file_record,json_read,read,require,write_new,bounded_process
from phase import check
import cache_lease
sys.path.insert(0,str(Q/'observer'));import observer as O
P=Q/'format-plan.json'; SHA='bef6ca891e07f277a85926cf736648a6f041731f9514c00c367af14f7222e0ff'
require(hashlib.sha256(P.read_bytes()).hexdigest()==SHA,'phase differs');plan=json_read(P);check(plan)
fd=cache_lease.open_bound(plan['lease'])
try:
 fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);cache_lease.check_held(fd,plan['lease'],SHA)
 require(not(Path(plan['lease']['state_directory'])/(SHA+'.json')).exists(),'already completed')
 original=file_record(Q/'controls/format/result.json'); r=json_read(original['path']);check_file(r['observer_result']);o=json_read(r['observer_result']['path'])
 require(r['accepted'] is False and r['observer_attempted'] is True and r['terminal_authenticated'] is False and r['raw_status']==137,'wrong refusal')
 require('TimeoutError: [Errno 110]' in o['observer_error'] and "'member': 'RefUnit'" in o['observer_error'],'wrong startup error')
 require(o['accounting_complete'] is False and o['comparison_eligible'] is False and o['sample_count']==0 and o['final_accounting'] is None,'unexpected accounting')
 require(not any(k in o for k in ['payload_authorization','authenticated_service','refusal_final_accounting']),'payload might have been released')
 absent=[str(Q/'observer/format'/name)for name in ['before-exec-receipt.json','payload.json','payload-exit.json']]
 require(all(not Path(p).exists() for p in absent),'payload record exists')
 launch=o['launch'];fields=O.report_fields(Q/'observer/format/safehermit.report');unit=O.authenticate_report(fields,launch)
 require(unit=='safehermit-20260918T083343Z-2735826.service','unit differs')
 require(fields['exec_main_code']=='2' and fields['exec_main_status']=='9' and fields['exit_code']=='137','helper terminal status differs')
 require(o['kill_commands'][0]['returncode']==0 and o['kill_commands'][0]['argv'][-1]==unit,'owned kill not successful')
 # The refused RefUnit precedes the sole authorization send in the authenticated observer.
 for name,sha in launch['observer_sources'].items():require(hashlib.sha256((Q/'observer'/name).read_bytes()).hexdigest()==sha,'observer source changed')
 identities=[launch['wrapper_identity'],dict(pid=int(fields['exec_main_pid']),start_ticks=int(fields['exec_main_start_monotonic_usec'])*os.sysconf('SC_CLK_TCK')//1000000)]
 checks=[]
 for i in range(2):
  query=bounded_process(['/usr/bin/systemctl','--user','show',unit,'--property=Id,LoadState,ActiveState,SubState,ControlGroup,MainPID,ExecMainPID,ExecMainStartTimestampMonotonic,CPUUsageNSec'],Path(plan['lease']['owner_slot']),plan['environment'],D/('service-'+str(i)),5,65536)
  require(not query['forced'],'query forced');v=dict(x.split('=',1)for x in read(query['stdout']['path']).decode().splitlines())
  require(query['returncode'] in [0,1] and v['Id']==unit and v['LoadState']=='not-found' and v['ActiveState']=='inactive' and v['SubState']=='dead' and v.get('ControlGroup','')=='' and v.get('MainPID','0')=='0','unit remains live')
  retired=[]
  for ident in identities:
   try:current=O.proc_identity(ident['pid'])
   except(FileNotFoundError,ProcessLookupError):current=None
   # A remaining numeric PID would require stronger generation proof; do not infer from timestamps.
   require(current is None,'original numeric PID still present')
   retired.append(dict(original=ident,current=None))
  checks.append(dict(query=query,properties=v,retired_processes=retired))
 check(plan);cache_lease.check_held(fd,plan['lease'],SHA)
 proof=dict(schema=1,kind='startup-refusal-lifecycle-only',accepted=False,observer_attempted=True,terminal_authenticated=True,accounting_complete=False,comparison_eligible=False,raw_status=137,original_result=original,observer_result=r['observer_result'],plan=file_record(P),fresh_checks=checks,absent_payload_records=absent,observer_sources=[file_record(Q/'observer'/n)for n in launch['observer_sources']],safehermit_report=file_record(Q/'observer/format/safehermit.report'),source_inputs_unchanged=True,payload_authorized=False,scope='Only owned phase quiescence and no authorization. No historical CPU accounting or format pass. Original result remains unqualified. Fresh continuation must explicitly recognize this recovery; original caller does not.')
 write_new(D/'PROOF.json',proof);print(file_record(D/'PROOF.json'))
finally:os.close(fd)
