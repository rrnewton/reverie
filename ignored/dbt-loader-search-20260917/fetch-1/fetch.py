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
plan=json.loads((E/'PLAN.json').read_text())
ca=json.loads((E/'CA.json').read_text());assert sha(pathlib.Path(ca['path']))==ca['sha256']
paths=json.loads((B/'source-paths.json').read_text());before={p:sha(S/p) for p in paths};save('source-before.json',before)
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=S,text=True).strip()=='a28de6f8eb585d7095fa80e5f4c0fc41ec3ba9e4'
for name in ['registry','git']:save(name+'-before.json',manifest(B/'cargo'/name))
assert not (S/'Cargo.lock').exists()
env=dict(os.environ);env.update(plan['env']);started=time.monotonic()
try:
 q=subprocess.run(plan['argv'],cwd=S,env=env);save('fetch-result.json',{'argv':plan['argv'],'env_delta':plan['env'],'actual_exit':q.returncode,'seconds':time.monotonic()-started})
 if (S/'Cargo.lock').exists():(E/'Cargo.lock').write_bytes((S/'Cargo.lock').read_bytes());save('generated-lock.json',{'sha256':sha(S/'Cargo.lock'),'bytes':(S/'Cargo.lock').stat().st_size})
 assert q.returncode==0,'normal fetch failed; preserve actual result'
finally:
 after={p:sha(S/p) for p in paths};save('source-after.json',after);assert after==before
 for name in ['registry','git']:save(name+'-after.json',manifest(B/'cargo'/name))
 assert sha(pathlib.Path(ca['path']))==ca['sha256']
