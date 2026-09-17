import hashlib,json,os,pathlib,stat,subprocess,time
E=pathlib.Path(__file__).parent
S=E.parents[2]
DONOR=pathlib.Path('/home/newton/work/dev-hermit/worktrees/slots/dev-hermit-dbt-loader-observer-20260916/ignored/hermetic/split/cargo')
def save(name,x):
 with (E/name).open('x') as f:json.dump(x,f,indent=2);f.write('\n')
def sha(p):
 h=hashlib.sha256()
 with p.open('rb') as f:
  for b in iter(lambda:f.read(1048576),b''):h.update(b)
 return h.hexdigest()
def manifest(root):
 out={}
 for p in [root,*sorted(root.rglob('*'))]:
  st=p.lstat();v={'mode':stat.S_IMODE(st.st_mode),'uid':st.st_uid,'device':st.st_dev,'inode':st.st_ino,'size':st.st_size,'mtime_ns':st.st_mtime_ns}
  if p.is_symlink():v.update(type='symlink',target=os.readlink(p))
  elif p.is_file():v.update(type='file',sha256=sha(p))
  elif p.is_dir():v.update(type='directory')
  else:raise RuntimeError('unsupported cache file type '+str(p))
  out[str(p.relative_to(root))]=v
 return out
source_paths=json.loads((E/'source-paths.json').read_text())
before={p:sha(S/p) for p in source_paths};save('source-before.json',before)
assert before['reverie-dbt/vendor/dynamorio/core/unix/loader.c']=='5e444f19b7d4c474120c83cea03d15124d01766e79b8c0b7741131606f283f59'
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=S,text=True).strip()=='a28de6f8eb585d7095fa80e5f4c0fc41ec3ba9e4'
forbidden=[k for k in os.environ if k.startswith(('LD_','DYNAMORIO_','REVERIE_DBT_')) or k in ('CMAKE','CMAKE_GENERATOR','RUSTC_WRAPPER','RUSTFLAGS','CC','CXX')];save('environment.json',{'forbidden_names_present':forbidden,'selected_toolchain':'nightly-2026-07-29','CI':os.environ.get('CI'),'CARGO_BUILD_JOBS':'4','CARGO_HOME':str(E/'cargo'),'CARGO_TARGET_DIR':str(E/'target')});assert not forbidden
(E/'cargo').mkdir();(E/'target').mkdir()
try:
 for name in ['registry','git']:
  a=manifest(DONOR/name);save(name+'-donor-before.json',a)
  command=['cp','-a','--reflink=auto',str(DONOR/name),str(E/'cargo'/name)];t=time.monotonic();q=subprocess.run(command);save(name+'-copy.json',{'argv':command,'actual_exit':q.returncode,'seconds':time.monotonic()-t});assert q.returncode==0
  b=manifest(DONOR/name);c=manifest(E/'cargo'/name);save(name+'-donor-after.json',b);save(name+'-copy-before.json',c);assert a==b
  for p,v in a.items():
   w=c[p];assert v['type']==w['type'] and v['mode']==w['mode']
   if v['type']=='file':assert v['sha256']==w['sha256'] and (v['device'],v['inode'])!=(w['device'],w['inode'])
   if v['type']=='symlink':assert v['target']==w['target'] and not str((E/'cargo'/name/p).resolve()).startswith(str(DONOR))
 env=dict(os.environ);env.update(CARGO_HOME=str(E/'cargo'),CARGO_TARGET_DIR=str(E/'target'),CARGO_BUILD_JOBS='4')
 assert not (S/'Cargo.lock').exists()
 for label,argv in [('generate-lock',['cargo','generate-lockfile','--offline']),('build',['cargo','build','--offline','--locked','-p','reverie-dbt','-j4'])]:
  print(label,argv,flush=True);t=time.monotonic();q=subprocess.run(argv,cwd=S,env=env);save(label+'-result.json',{'argv':argv,'actual_exit':q.returncode,'seconds':time.monotonic()-t});assert q.returncode==0,label+' failed'
  if label=='generate-lock':save('generated-lock.json',{'sha256':sha(S/'Cargo.lock'),'bytes':(S/'Cargo.lock').stat().st_size});(E/'Cargo.lock').write_bytes((S/'Cargo.lock').read_bytes())
 installs=list((E/'target/debug/reverie-dbt-native-cache').glob('dynamorio-install-*'));assert len(installs)==1
 install=installs[0];assert install.name!='dynamorio-install-c9c1ee55257cbb0635b56f494a75ee1dc6af839ca8e289231f533b0208340463'
 save('native-install.json',{'path':str(install),'entries':manifest(install)})
 print('actual candidate native install',install,flush=True)
finally:
 after={p:sha(S/p) for p in source_paths};save('source-after.json',after);assert before==after
