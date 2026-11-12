import hashlib,json,os,pathlib,subprocess,time
E=pathlib.Path(__file__).parent;S=E.parents[2];B=E.parent/'native-build-3'
def save(n,x):
 with (E/n).open('x') as f:json.dump(x,f,indent=2);f.write('\n')
env=dict(os.environ);env.update(TG_DB_PATH='/home/newton/.tg/hermit2.db',CARGO_HOME=str(B/'cargo'),CARGO_TARGET_DIR=str(B/'target'),CARGO_BUILD_JOBS='4',TMPDIR=str(B/'tmp'))
def call(label,argv):
 t=time.monotonic();p=subprocess.run(argv,cwd=S,env=env,capture_output=True,text=True);save(label+'.json',dict(argv=argv,actual_exit=p.returncode,seconds=time.monotonic()-t,stdout=p.stdout,stderr=p.stderr));assert p.returncode==0,label;return p.stdout
rows=json.loads((E/'source.json').read_text());paths=[r['path'] for r in rows]
def guard():
 for r in rows:assert hashlib.sha256((S/r['path']).read_bytes()).hexdigest()==r['sha256'],r['path']
guard()
assert call('head-before',['git','rev-parse','HEAD']).strip()=='24cd5bb518b027eddb62a226805210d74d31c3d8'
assert call('branch-before',['git','branch','--show-current']).strip()=='dev-hermit/sabre-frame-abi-20260917'
assert not call('index-before',['git','diff','--cached','--name-only']).strip()
changed=call('tracked-before',['git','diff','--name-only']).splitlines();expected=[p for p in paths if not p.startswith('experimental/reverie-sabre/tests/')];assert sorted(changed)==sorted(expected)
untracked=call('untracked-before',['git','ls-files','--others','--exclude-standard']).splitlines();assert sorted(untracked)==sorted(set(paths)-set(expected));assert not (S/'HANDOFF.md').exists()
assert call('remote-main',['with-proxy','git','ls-remote','https://github.com/rrnewton/reverie.git','refs/heads/main']).split()[0]=='24cd5bb518b027eddb62a226805210d74d31c3d8'
call('fmt',['cargo','fmt','--all','--','--check'])
call('clippy',['cargo','clippy','--offline','--locked','-p','reverie-sabre','--all-targets','--','-D','warnings'])
guard()
tag=call('who-am-i',['/home/newton/work/dev-hermit/ci-hub/bin/who-am-i','--tag','--role','impl','--task','hermit-main-tip-d0b1468b-not-validated-22-nodes-mostly-exit-2','--repo-root','/home/newton/work/dev-hermit','--db','/home/newton/.tg/hermit2.db']).strip();assert tag.startswith('[') and tag.endswith(']') and '\n' not in tag
body='Repair SaBRe syscall frames and child continuations\n\n'+tag+'''\n\nPlain Language Summary
The x86 syscall wrapper saves RFLAGS before the register frame, but its Rust/C declarations and fork-form child epilogues omitted that word. A child selected the architectural return instead of the scratch continuation, bypassing displaced instructions and resuming with the stack128+8 bytes low. Match the real18-word frame, return through scratch, preserve original flags, and make the loader entry and register readers use the same layout.

Project Impact
Preserves original fork/clone assertions and the separate RISC-V layout. New native tests exercise actual wrapper assembler, C accessors and real Rust fork/vfork child-return helpers, with exact stack, scratch, displaced instruction, flags and all128 red-zone bytes. Existing vfork and register-reader tests also pass. The CFI correction changes unwind metadata only; emitted loader instruction bytes are identical. The existing SIGILL bridge remains a return-pointer-only path; no full-frame guarantee for that bridge, kernel clone3 execution, new-stack clone register parity, Hermit guest success or full-main validation is claimed here.

Validation: old actual-wrapper12/12 failures versus corrected12/12 passes; three real Rust integration methods across both wrappers/two flag patterns; existing vfork and register-reader methods; genuine Cargo build.rs/native build, ordinary fmt and relevant all-target Clippy. Exact-child wait preserves failure status, and flushing fixture stdout preserves complete raw observations. No assertion, case, comparator, timeout or failure classification was relaxed. Historical regular-sink RIP0 attribution remains source/stack evidence, not a captured last-branch trace.

Critical review: trigger2 applies to the public frame/interception ABI. Independent native and actual Claude source reviews/manual publication gate are required; this local checkpoint is not approval or publication.

Task: hermit-main-tip-d0b1468b-not-validated-22-nodes-mostly-exit-2
'''
(E/'message.txt').write_text(body)
call('add',['git','add','--',*paths]);assert sorted(call('staged',['git','diff','--cached','--name-only']).splitlines())==sorted(paths)
call('diff-check',['git','diff','--cached','--check']);call('commit',['git','commit','-F',str(E/'message.txt')])
head=call('head-after',['git','rev-parse','HEAD']).strip();tree=call('tree-after',['git','rev-parse','HEAD^{tree}']).strip();assert sorted(call('commit-paths',['git','diff-tree','--no-commit-id','--name-only','-r','HEAD']).splitlines())==sorted(paths)
guard();assert not call('status-after',['git','status','--porcelain=v1','--untracked-files=all']).strip()
save('COMMITTED.json',dict(head=head,tree=tree,parent='24cd5bb518b027eddb62a226805210d74d31c3d8',source=rows,disclosure=tag));print(head,tree,flush=True)
