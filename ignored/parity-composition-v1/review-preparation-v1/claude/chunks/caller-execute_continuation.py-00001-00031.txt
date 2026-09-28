from pathlib import Path
import fcntl,hashlib,json,os,subprocess,sys,time
import cache_lease
from common import check_file,digest,file_record,json_read,require,write_new
Q=Path(__file__).resolve().parent
require(sys.flags.optimize==0 and os.environ.get('PYTHONOPTIMIZE')=='0','unoptimized caller required')
c=json_read(Q/'CONTINUATION.json');old=json_read(c['old_plan']);require(digest(c['old_plan'])==c['old_plan_sha256'],'old plan differs')
for row in json_read(Q/'CALLER-INPUTS.json')['records']:
 p=Path(row['path'])
 if row.get('kind')=='symlink':require(p.is_symlink() and os.readlink(p)==row['target'],'alias changed')
 else:check_file(row)
fd=cache_lease.open_bound(old['lease']);fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);cache_lease.check_held(fd,old['lease'],c['old_plan_sha256'])
try:
 for name in sys.argv[1:]:
  require(name in c['sequence'],'unknown phase')
  for step,args in [('prepare',['/usr/bin/python3','-B',str(Q/'prepare.py'),name]),('launch',None)]:
   if args is None:
    plan=Q/(name+'-plan.json');args=['/usr/bin/python3','-B',str(Q/'phase.py'),'launch',str(plan),digest(plan)]
   start=time.monotonic();prefix=Q/'launch'/(name+'-'+step)
   with prefix.with_suffix('.stdout').open('xb')as out,prefix.with_suffix('.stderr').open('xb')as err:
    r=subprocess.run(args,cwd=Q,stdout=out,stderr=err,pass_fds=(fd,))
   write_new(prefix.with_suffix('.json'),dict(argv=args,raw_status=r.returncode,elapsed_seconds=time.monotonic()-start,stdout=file_record(prefix.with_suffix('.stdout')),stderr=file_record(prefix.with_suffix('.stderr'))))
   print(json.dumps(dict(phase=name,step=step,raw_status=r.returncode,elapsed_seconds=time.monotonic()-start)),flush=True)
   if r.returncode:raise SystemExit(r.returncode)
  result=json_read(Q/'controls'/name/'result.json');require(result['accepted']and result['terminal_authenticated']and result['inputs_unchanged']and result['raw_status']==0,'phase refused')
  if name in ('metadata','compile'):
   helper='bind_dependencies.py'if name=='metadata'else'retain_artifacts.py';prefix=Q/'launch'/helper.removesuffix('.py')
   with prefix.with_suffix('.stdout').open('xb')as out,prefix.with_suffix('.stderr').open('xb')as err:r=subprocess.run(['/usr/bin/python3','-B',str(Q/helper)],cwd=Q,stdout=out,stderr=err,pass_fds=(fd,))
   write_new(prefix.with_suffix('.json'),dict(raw_status=r.returncode,stdout=file_record(prefix.with_suffix('.stdout')),stderr=file_record(prefix.with_suffix('.stderr'))));require(r.returncode==0,'post-phase binding refused')
finally:
 cache_lease.check_held(fd,old['lease'],c['old_plan_sha256']);os.close(fd)
