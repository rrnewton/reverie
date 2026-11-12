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
B=E.parent/'native-build-2';C=E.parent/'native-build-1/cargo'
paths=json.loads((E.parent/'native-build-1/source-paths.json').read_text());before={p:sha(S/p) for p in paths};save('source-before.json',before)
assert before['reverie-dbt/vendor/dynamorio/core/unix/loader.c']=='47519a323785ea317c674062da6e12d7a3dc08de6205c7d44d7fee0248aec4a8'
assert before['reverie-dbt/tests/private_loader_search.rs']=='71e4dcd7c1263fd48527d960205ffdd23f7152ea1fc7a0f06d466819da0d5a19'
assert sha(S/'Cargo.lock')=='301cccef3def9b09308f66c7349b47f04c794e5151fe07f1820b497d3890d8f1'
(E/'tmp').mkdir(mode=0o700);env=dict(os.environ);env.update(CARGO_HOME=str(C),CARGO_TARGET_DIR=str(B/'target'),CARGO_BUILD_JOBS='4',TMPDIR=str(E/'tmp'))
save('environment.json',{'env_delta':{k:env[k] for k in ['CARGO_HOME','CARGO_TARGET_DIR','CARGO_BUILD_JOBS','TMPDIR']}})
commands=[('list',['cargo','test','--offline','--locked','-p','reverie-dbt','--test','private_loader_search','--','--list']),('tests',['cargo','test','--offline','--locked','-p','reverie-dbt','--test','private_loader_search','--','--test-threads=1','--nocapture']),('fmt',['cargo','fmt','--all','--','--check']),('clippy',['cargo','clippy','--offline','--locked','-p','reverie-dbt','--all-targets','--all-features'])]
save('commands.json',commands)
try:
 for label,argv in commands:
  print(label,argv,flush=True);t=time.monotonic()
  with (E/(label+'.stdout')).open('xb') as out,(E/(label+'.stderr')).open('xb') as err:q=subprocess.run(argv,cwd=S,env=env,stdout=out,stderr=err)
  save(label+'-result.json',{'argv':argv,'actual_exit':q.returncode,'seconds':time.monotonic()-t})
  print(label,'actual',q.returncode,flush=True)
  if q.returncode!=0:print((E/(label+'.stderr')).read_text()[-10000:],flush=True)
  assert q.returncode==0,label+' failed'
  if label=='list':
   text=(E/'list.stdout').read_text();names=[line.removesuffix(': test') for line in text.splitlines() if line.endswith(': test')];save('collection.json',names);assert len(names)==10 and len(set(names))==10,text
finally:
 after={p:sha(S/p) for p in paths};save('source-after.json',after);assert before==after
 assert sha(S/'Cargo.lock')=='301cccef3def9b09308f66c7349b47f04c794e5151fe07f1820b497d3890d8f1'
