from pathlib import Path
import subprocess,os,json,time,sys,hashlib
s=Path(__file__).resolve().parents[3];p=Path(__file__).resolve().parent;e=p.parent
expected='17982ed4903b8097f6e0451fc02338595c89f3d7'
def git(*args):return subprocess.check_output(['git',*args],cwd=s)
assert git('rev-parse','HEAD').decode().strip()==expected
assert not git('status','--porcelain','--untracked-files=no')
branch='refs/heads/dev-hermit/gdb-vfile-unsupported-20260916'
before=subprocess.run(['git','ls-remote','origin',branch],cwd=s,capture_output=True,text=True);assert before.returncode==0 and not before.stdout.strip()
(p/'remote-before.json').write_text(json.dumps({'actual_exit':before.returncode,'stdout':before.stdout,'stderr':before.stderr})+'\n')
env=os.environ.copy();env.update(CARGO_HOME=str(e/'cargo'),CARGO_TARGET_DIR=str(e/'target-new-1'),CARGO_BUILD_JOBS='4',CARGO_HTTP_CAINFO='/etc/pki/tls/certs/fb_certs.pem',RUSTFLAGS='-C link-arg=-llzma')
argv=['/home/newton/work/dev-hermit/ci-hub/bin/git-push-verified','origin','HEAD:'+branch]
t=time.monotonic()
with (p/'push.stdout').open('wb') as stdout,(p/'push.stderr').open('wb') as stderr:r=subprocess.run(argv,cwd=s,env=env,stdout=stdout,stderr=stderr)
row={'argv':argv,'actual_exit':r.returncode,'seconds':time.monotonic()-t,'head_after':git('rev-parse','HEAD').decode().strip(),'tracked_status_after':git('status','--porcelain','--untracked-files=no').decode()};(p/'push-result.json').write_text(json.dumps(row,indent=2)+'\n');print(json.dumps(row),flush=True);print((p/'push.stdout').read_text());print((p/'push.stderr').read_text());sys.exit(r.returncode)
