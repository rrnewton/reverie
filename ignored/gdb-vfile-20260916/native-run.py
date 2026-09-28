import hashlib,json,os,subprocess,sys,time
from pathlib import Path
s=Path(__file__).resolve().parents[2]
e=Path(__file__).resolve().parent
variant=sys.argv[1]
out=e/variant;out.mkdir(exist_ok=False)
paths=subprocess.check_output(['git','ls-files','-z'],cwd=s).decode().split('\0')
def snapshot():
 return {'head':subprocess.check_output(['git','rev-parse','HEAD'],cwd=s,text=True).strip(), 'files':{p:hashlib.sha256((s/p).read_bytes()).hexdigest() for p in paths if p and (s/p).is_file() and not (s/p).is_symlink()}, 'diff':hashlib.sha256(subprocess.check_output(['git','diff','--binary','--full-index'],cwd=s)).hexdigest()}
def put(name,data): (out/name).write_text(json.dumps(data,indent=2)+'\n')
before=snapshot();put('source-before.json',before)
stat=Path('/proc/self/stat').read_text();gen=stat[stat.rfind(')')+2:].split()[19]
cg=Path('/proc/self/cgroup').read_text().split('0::',1)[1].strip();cgpath=Path('/sys/fs/cgroup')/cg.lstrip('/')
put('identity.json',{'pid':os.getpid(),'start':gen,'cgroup':str(cgpath),'inode':cgpath.stat().st_ino})
def resources():
 return {p:(cgpath/p).read_text() for p in ['memory.max','memory.swap.max','memory.peak','memory.events','pids.max','pids.peak','pids.events','cpu.max','cpu.stat'] if (cgpath/p).exists()}
put('resources-before.json',resources())
env=os.environ.copy();env.update(CARGO_HOME=str(e/'cargo'),CARGO_TARGET_DIR=str(e/('target-'+variant)),CARGO_BUILD_JOBS='4',CARGO_HTTP_CAINFO='/etc/pki/tls/certs/fb_certs.pem',RUSTFLAGS='-C link-arg=-llzma')
records=[]
def run(name,argv):
 t=time.monotonic()
 with (out/(name+'.stdout')).open('wb') as stdout,(out/(name+'.stderr')).open('wb') as stderr:
  p=subprocess.run(argv,cwd=s,env=env,stdout=stdout,stderr=stderr)
 row={'name':name,'argv':argv,'actual_exit':p.returncode,'seconds':time.monotonic()-t};records.append(row);put('commands.json',records);print(json.dumps(row),flush=True);return p.returncode
cargo='/home/newton/.cargo/bin/cargo'
rc=run('compile',[cargo,'test','--locked','--offline','-p','reverie-ptrace','--lib','--no-run','--jobs','4','--message-format=json'])
if rc==0:
 artifacts=[]
 for line in (out/'compile.stdout').read_text().splitlines():
  try: j=json.loads(line)
  except ValueError: continue
  if j.get('reason')=='compiler-artifact' and j.get('executable') and j.get('target',{}).get('name')=='reverie_ptrace' and j.get('profile',{}).get('test'): artifacts.append(j)
 assert len(artifacts)==1,artifacts
 binary=Path(artifacts[0]['executable']);digest=hashlib.sha256(binary.read_bytes()).hexdigest();put('compiled-executable.json',{'artifact':artifacts[0],'sha256':digest})
 rc=run('gdbstub-tests',[str(binary),'gdbstub::','--nocapture','--test-threads=1'])
 assert digest==hashlib.sha256(binary.read_bytes()).hexdigest()
 run('inventory',[str(binary),'gdbstub::','--list'])
 if variant.startswith('new') and rc==0:
  rc=run('clippy',[cargo,'clippy','--locked','-p','reverie-ptrace','--lib','--tests','--','-D','warnings'])
  if rc==0: rc=run('format',['/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin/rustfmt','--check','--edition','2024','reverie-ptrace/src/gdbstub/commands/base/_vFile.rs','reverie-ptrace/src/gdbstub/session.rs','reverie-ptrace/src/gdbstub/server.rs'])
after=snapshot();put('source-after.json',after);put('resources-final.json',resources());put('result.json',{'actual_last_exit':rc,'source_unchanged':before==after,'commands':records,'cargo_lock_sha256':hashlib.sha256((s/'Cargo.lock').read_bytes()).hexdigest() if (s/'Cargo.lock').exists() else None});assert before==after
sys.exit(rc)
