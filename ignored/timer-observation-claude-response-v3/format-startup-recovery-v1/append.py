from pathlib import Path
import fcntl,hashlib,os,sys
D=Path(__file__).resolve().parent;Q=D.parent/'positive-v2';sys.path.insert(0,str(Q))
from common import check_file,file_record,json_read,require,write_new
from phase import check
import cache_lease
p=D/'PROOF.json';require(hashlib.sha256(p.read_bytes()).hexdigest()=='d240c378035634468803f123d816b5da9937837892709efbb57a35d4c95b84f9','recovery changed');r=json_read(p);check_file(r['plan']);plan=json_read(r['plan']['path']);check(plan);fd=cache_lease.open_bound(plan['lease'])
try:
 fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);sha=r['plan']['sha256'];cache_lease.check_held(fd,plan['lease'],sha)
 for record in [r['original_result'],r['observer_result'],r['safehermit_report'],*r['observer_sources']]:check_file(record)
 require(r['accepted'] is False and r['payload_authorized'] is False and r['accounting_complete'] is False,'wrong scope')
 cache_lease.complete(fd,plan['lease'],sha,p)
 write_new(D/'COMPLETION.json',dict(completion=file_record(Path(plan['lease']['state_directory'])/(sha+'.json')),proof=file_record(p),source_unchanged=True,scope='Lifecycle-only completion; original observer attempt and raw137 preserved. No formatting pass or retrospective CPU accounting.'))
finally:os.close(fd)
