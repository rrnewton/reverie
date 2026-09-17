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
B=E.parent/'native-build-1'
paths=json.loads((B/'source-paths.json').read_text());before={p:sha(S/p) for p in paths};save('source-before.json',before)
assert before['reverie-dbt/vendor/dynamorio/core/unix/loader.c']=='47519a323785ea317c674062da6e12d7a3dc08de6205c7d44d7fee0248aec4a8'
assert sha(S/'Cargo.lock')=='301cccef3def9b09308f66c7349b47f04c794e5151fe07f1820b497d3890d8f1'
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=S,text=True).strip()=='a28de6f8eb585d7095fa80e5f4c0fc41ec3ba9e4'
forbidden=[k for k in os.environ if k.startswith(('LD_','DYNAMORIO_','REVERIE_DBT_')) or k in ('CMAKE','CMAKE_GENERATOR','RUSTC_WRAPPER','RUSTFLAGS','CC','CXX')];save('environment.json',{'forbidden_names_present':forbidden,'CI':os.environ.get('CI'),'env_delta':{'CARGO_HOME':str(B/'cargo'),'CARGO_TARGET_DIR':str(E/'target'),'CARGO_BUILD_JOBS':'4'}});assert not forbidden
(E/'target').mkdir()
env=dict(os.environ);env.update(CARGO_HOME=str(B/'cargo'),CARGO_TARGET_DIR=str(E/'target'),CARGO_BUILD_JOBS='4')
argv=['cargo','build','--offline','--locked','-p','reverie-dbt','-j4'];print(argv,flush=True);started=time.monotonic()
try:
 q=subprocess.run(argv,cwd=S,env=env);save('build-result.json',{'argv':argv,'actual_exit':q.returncode,'seconds':time.monotonic()-started});assert q.returncode==0,'actual targeted build failed'
 installs=list((E/'target/debug/reverie-dbt-native-cache').glob('dynamorio-install-*'));assert len(installs)==1
 install=installs[0];assert install.name!='dynamorio-install-c9c1ee55257cbb0635b56f494a75ee1dc6af839ca8e289231f533b0208340463'
 save('native-install.json',{'path':str(install),'entries':manifest(install)})
 print('actual candidate install',install,flush=True)
finally:
 after={p:sha(S/p) for p in paths};save('source-after.json',after);assert after==before
 assert sha(S/'Cargo.lock')=='301cccef3def9b09308f66c7349b47f04c794e5151fe07f1820b497d3890d8f1'
