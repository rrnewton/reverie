from pathlib import Path
import hashlib,json,subprocess,time,os,selectors,sys
root=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918')
qualified=root/'ignored/kvm-entry-remaining-controls-v3';sys.path.insert(0,str(qualified))
from runner import service_show,stop_service
out=root/'ignored/kvm-entry-lint-v2';out.mkdir()
source=qualified/'source-1.json';m=json.loads(source.read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
def check():
 for n,r in m['source'].items():assert sha(root/n)==r['sha256'],n
check();old=root/'ignored/kvm-entry-lint-v1/clippy/command.json';cmd=json.loads(old.read_text())['argv'];cmd=[x for x in cmd if not x.startswith('--property=LimitFSIZE=')];unit='kvm-entry-clippy-v2-'+str(os.getpid());cmd[cmd.index('--unit')+1]=unit
(out/'command.json').write_text(json.dumps({'argv':cmd,'source_manifest':str(source),'source_sha256':sha(source),'stream_cap_bytes':16777216,'predecessor_refusal':'v1 clippy killed by SIGXFSZ because global LimitFSIZE accidentally capped compiler metadata, before KVM lint; v2 caps output streams only','unchanged_helpers':{n:sha(qualified/n) for n in ['runner.py','support.py']}},indent=2)+'\n')
start=time.monotonic();p=subprocess.Popen(cmd,cwd=root,stdout=subprocess.PIPE,stderr=subprocess.PIPE);sel=selectors.DefaultSelector();streams={};counts={};controls=[];refusals=[]
for name,pipe in [('stdout',p.stdout),('stderr',p.stderr)]:sel.register(pipe,selectors.EVENT_READ,name);streams[name]=(out/name).open('xb');counts[name]=0
while sel.get_map():
 for key,_ in sel.select(.25):
  b=os.read(key.fileobj.fileno(),65536)
  if not b:sel.unregister(key.fileobj);key.fileobj.close();continue
  name=key.data;remaining=max(0,16777216-counts[name]);streams[name].write(b[:remaining]);counts[name]+=len(b)
  if counts[name]>16777216 and not refusals:refusals.append('stream cap');stop_service(unit,controls)
 if time.monotonic()-start>625 and not refusals:refusals.append('outer deadline');stop_service(unit,controls)
for f in streams.values():f.close()
rc=p.wait(timeout=15);terminal=service_show(unit);check();result={'raw_status':rc,'elapsed_seconds':time.monotonic()-start,'source_unchanged':True,'refusals':refusals,'controls':controls,'terminal':terminal,'streams':{n:{'bytes_seen':counts[n],'stored_sha256':sha(out/n)} for n in streams}}
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));raise SystemExit(rc if rc else bool(refusals))
