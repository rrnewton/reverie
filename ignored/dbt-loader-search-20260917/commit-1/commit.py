import hashlib,json,os,pathlib,subprocess,time
E=pathlib.Path(__file__).parent;S=E.parents[2]
def save(n,x):
 with (E/n).open('x') as f:json.dump(x,f,indent=2);f.write('\n')
def call(label,argv):
 t=time.monotonic();p=subprocess.run(argv,cwd=S,env=env,capture_output=True,text=True);save(label+'.json',{'argv':argv,'actual_exit':p.returncode,'seconds':time.monotonic()-t,'stdout':p.stdout,'stderr':p.stderr});assert p.returncode==0,label;return p.stdout
paths=json.loads((E/'paths.json').read_text());manifest=json.loads((E.parent/'CANDIDATE-WITH-TESTS-4.json').read_text());env=dict(os.environ);env['TG_DB_PATH']='/home/newton/.tg/hermit2.db'
assert call('head-before',['git','rev-parse','HEAD']).strip()=='a28de6f8eb585d7095fa80e5f4c0fc41ec3ba9e4'
assert call('branch-before',['git','branch','--show-current']).strip()=='dev-hermit/dbt-loader-search-20260917'
assert not call('index-before',['git','diff','--cached','--name-only']).strip()
status=call('status-before',['git','status','--porcelain=v1','--untracked-files=normal'])
changed=call('tracked-before',['git','diff','--name-only']).splitlines();assert changed==['reverie-dbt/vendor/dynamorio/core/unix/loader.c']
for p,v in manifest['files'].items():assert hashlib.sha256((S/p).read_bytes()).hexdigest()==v['sha256'],p
assert not (S/'HANDOFF.md').exists()
remote=call('remote-main',['with-proxy','git','ls-remote','https://github.com/rrnewton/reverie.git','refs/heads/main']);assert remote.split()[0]=='a28de6f8eb585d7095fa80e5f4c0fc41ec3ba9e4'
tag=call('who-am-i',['/home/newton/work/dev-hermit/ci-hub/bin/who-am-i','--tag','--role','impl','--task','vision-ci-signal-is-trustworthy-end-to-end','--repo-root','/home/newton/work/dev-hermit','--db','/home/newton/.tg/hermit2.db']).strip();assert tag.startswith('[') and tag.endswith(']') and '\n' not in tag
body='Fix private-loader lookup of later direct dependencies\n\n'+tag+'\n\nPlain Language Summary\nA client can directly depend on both a parent library and a later leaf library. When an existing path resolves the parent first, DynamoRIO descends into it before checking the client RUNPATH and can fail to locate the leaf. After all existing searches fail, consult the client RUNPATH only for a directly declared SONAME, without promoting paths globally.\n\nProject Impact\nPreserves successful search precedence, missing and indirect-only failures, and explicit dependency pathnames. Ten normal Linux/x86_64 integration tests exercise the real private loader and native loader with exact ELF dependency order and provider markers. Native controls change the original direct-leaf failure from255 to0 while retaining negative failures. This does not claim Hermit DBT or full-main validation success.\n\nValidation: actual10/10 integration tests; cargo fmt --all -- --check; relevant cargo clippy --all-targets --all-features; genuine build.rs native source key0aa6d842. Preserve the initial offline missing-index refusal and the first test fixture refusal of Cargo LD_LIBRARY_PATH; the fixture now accepts only that inherited variable because every child explicitly replaces it. No original test, timeout, comparison or failure classification was relaxed.\n\nTask: vision-ci-signal-is-trustworthy-end-to-end\n'
(E/'message.txt').write_text(body)
call('add',['git','add','--',*paths]);staged=call('index-staged',['git','diff','--cached','--name-only']).splitlines();assert sorted(staged)==sorted(paths)
call('diff-check',['git','diff','--cached','--check'])
call('commit',['git','commit','-F',str(E/'message.txt')])
head=call('head-after',['git','rev-parse','HEAD']).strip();tree=call('tree-after',['git','rev-parse','HEAD^{tree}']).strip();actual=call('commit-paths',['git','diff-tree','--no-commit-id','--name-only','-r','HEAD']).splitlines();assert sorted(actual)==sorted(paths)
assert not call('tracked-after',['git','diff','--name-only']).strip();assert not call('index-after',['git','diff','--cached','--name-only']).strip()
for p,v in manifest['files'].items():assert hashlib.sha256((S/p).read_bytes()).hexdigest()==v['sha256'],p
save('COMMITTED.json',{'head':head,'tree':tree,'parent':'a28de6f8eb585d7095fa80e5f4c0fc41ec3ba9e4','paths':paths,'source_files':manifest['files'],'disclosure':tag,'remaining_untracked':call('status-after',['git','status','--porcelain=v1','--untracked-files=normal'])});print(head,tree,flush=True)
