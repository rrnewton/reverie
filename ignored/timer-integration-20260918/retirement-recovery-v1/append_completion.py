from pathlib import Path
import fcntl,hashlib,os,sys
D=Path(__file__).resolve().parent;T=D.parent/'qualification-v3';sys.path.insert(0,str(T));from common import check_file,file_record,json_read,require,write_new
import cache_lease
from phase import check as check_source
p=D/'LIFECYCLE-RECOVERY.json';require(hashlib.sha256(p.read_bytes()).hexdigest()=='bfa9fde41cc52e989b3a173ffc9bdc273b73c0acccf3e9adefe75f09c5e20fba','recovery identity');r=json_read(p);check_file(r['original_plan']);plan=json_read(r['original_plan']['path']);C=plan;check_source(C);fd=cache_lease.open_bound(C['lease'])
try:
 fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);sha=r['original_plan']['sha256'];cache_lease.check_held(fd,C['lease'],sha);check_file(r['original_unqualified_result']);check_file(r['observer_result']);require(r['accepted']is False and r['terminal_authenticated']is True and r['observer_attempted']is True,'lifecycle-only flags changed');require(all(x['cgroup_absent']and x['properties']['LoadState']=='not-found' for x in r['fresh_quiescence_checks']),'missing original quiescence proof');cache_lease.complete(fd,C['lease'],sha,p);completion=Path(C['lease']['state_directory'])/(sha+'.json');write_new(D/'LIFECYCLE-COMPLETION.json',dict(completion=file_record(completion),recovery=file_record(p),source_unchanged=True,scope='Append-only lifecycle completion under the unchanged owned lease token. Original attempt remains unqualified and ran; no new payload yet.'));print(file_record(completion))
finally:os.close(fd)
