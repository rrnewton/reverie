#!/usr/bin/python3
"""Create/freeze only the separately authorized two-test-source corrections."""
import difflib,hashlib,json,os,shutil,stat
from pathlib import Path
D=Path(__file__).resolve().parent
B=D.parent/'kvm-capture-pipe-identity-source-v1'
V=D.parent/'rdtsc-recovery-source-v6'
F=B/'qualification-v1/failure-final-v1'
def require(ok,msg):
    if not ok:raise RuntimeError(msg)
def rec(p):
    p=Path(p);s=p.lstat();d=dict(path=str(p),file_mode=stat.S_IMODE(s.st_mode))
    if stat.S_ISREG(s.st_mode):d.update(kind='file',bytes=s.st_size,sha256=hashlib.sha256(p.read_bytes()).hexdigest())
    elif stat.S_ISLNK(s.st_mode):d.update(kind='symlink',target=os.readlink(p),sha256=hashlib.sha256(os.fsencode(os.readlink(p))).hexdigest())
    else:require(stat.S_ISDIR(s.st_mode)and not any(p.iterdir()),'unexpected directory');d.update(kind='unexpanded_gitlink')
    return d
def verify(row):
    a=rec(row['path'])
    for k in ['kind','bytes','sha256','target','file_mode']:
        if k in row:require(a.get(k)==row[k],'changed '+row['path']+' '+k)
    return a
def write(name,v):
    p=D/name
    with p.open('x')as f:json.dump(v,f,indent=2);f.write('\n')
original=json.loads((B/'SOURCE-MANIFEST.json').read_text())
require(len(original)==2626,'old source count')
for row in original:
    verify(row);p=D/'source'/row['relative'];p.parent.mkdir(parents=True,exist_ok=True)
    if row['kind']=='unexpanded_gitlink':p.mkdir()
    elif row['kind']=='symlink':p.symlink_to(row['target'])
    else:shutil.copy2(row['path'],p);require(p.stat().st_ino!=Path(row['path']).stat().st_ino,'hardlinked source')
p=D/'source/reverie-kvm/src/capture_identity_tests.rs';s=p.read_text();old='''    async fn init_global_state(_: &()) -> Self {
        panic!("failed capture setup initialized the Tool");
    }
''';new=old+'''    async fn receive_rpc(&self, _: reverie::Tid, _: ()) {
        panic!("failed capture setup dispatched an RPC");
    }
''';require(s.count(old)==1,'GlobalTool insertion ambiguous');p.write_text(s.replace(old,new))
p=D/'source/reverie-kvm/src/executor.rs';s=p.read_text();start=s.index('fn proc_executable_link_marks_unlinked_and_replaced_files_deleted()');tail=s[start:];old='''                4096,
                false,
            );''';new='''                4096,
                None,
            );''';require(tail.count(old)==1,'existing test argument ambiguous');p.write_text(s[:start]+tail.replace(old,new,1))
current=[];changes=[]
for row in original:
    now=rec(D/'source'/row['relative']);now.update(relative=row['relative'],mode=row['mode']);current.append(now)
    if now.get('sha256')!=row.get('sha256'):changes.append(row['relative'])
require(changes==['reverie-kvm/src/capture_identity_tests.rs','reverie-kvm/src/executor.rs'],'unexpected source changes')
write('SOURCE-MANIFEST.json',current)
patch=[]
for rel in changes:
    a=(B/'source'/rel).read_text();b=(D/'source'/rel).read_text()
    patch.append('diff --git a/'+rel+' b/'+rel+'\n');patch.extend(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='a/'+rel,tofile='b/'+rel))
    for label,root in [('before',B),('after',D)]:
        dest=D/label/rel;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(root/'source'/rel,dest)
(D/'V1-TO-V2.patch').write_text(''.join(patch))
base={r['relative']:r for r in json.loads((V/'SOURCE-MANIFEST.json').read_text())};full=[];paths=[]
for row in current:
    rel=row['relative'];old=base.get(rel)
    if old and row.get('sha256')==old.get('sha256'):continue
    require(row['kind']=='file','unexpected changed nonfile');paths.append(rel)
    a=(V/'source'/rel).read_text()if old else '';b=Path(row['path']).read_text()
    full.append('diff --git a/'+rel+' b/'+rel+'\n')
    if not old:full.append('new file mode 100644\n')
    full.extend(difflib.unified_diff(a.splitlines(True),b.splitlines(True),fromfile='a/'+rel if old else '/dev/null',tofile='b/'+rel))
require(len(paths)==7,'full B scope changed');(D/'SOURCE.patch').write_text(''.join(full))
shutil.copy2(B/'SELECTORS.json',D/'SELECTORS.json')
write('SOURCE-CONTINUITY.json',dict(v1_target=rec(B/'TARGET.json'),v1_manifest=rec(B/'SOURCE-MANIFEST.json'),v1_patch=rec(B/'SOURCE.patch'),v1_failed_readback=rec(F/'READBACK.json'),v1_failed_results=rec(F/'RESULTS.json'),v6_target=rec(V/'TARGET.json'),v6_full_evidence=rec(V/'final-v1/TARGET.json'),product_entries=2626,changed_from_v1=changes,remaining2624identical=True,production_bytes_identical_to_v1=True,selectors_identical=rec(D/'SELECTORS.json'),full_paths_against_v6=paths))
(D/'REPORT.md').write_text('''# Capture pipe identity V2: two compile corrections only

This unexecuted successor fixes the two actual V1 compile errors. It does not change production behavior. V1 metadata passed; V1 compile failed raw 101, before any of the 63 declarations ran. Its complete failure and private lease release remain unchanged in the prior packet.

The required GlobalTool::receive_rpc implementation now panics if called. This completes the test-only trait contract and strengthens its existing requirement: a capture allocation failure must return before Tool initialization or RPC dispatch. init_global_state still panics. The real EMFILE controls still require the original image, no recorded Tool failure, exact recovered descriptor counts and actual first/second pipe and relocation paths. No successful setup or RPC is substituted.

The old proc_executable_link_marks_unlinked_and_replaced_files_deleted test now passes None to readlink_at_impl, the exact no-capture meaning of its old false value. This /proc/self/exe path obtains the opened executable's real fd/path; it does not use capture metadata. All deleted/replaced-file, output-buffer and size assertions are byte-identical. No selector, timeout, comparator or result gate changes.

All other 2,624 product entries, including every production line, are identical to V1. The complete seven-path patch against qualified V6 and exact two-path V1-to-V2 delta are supplied. The source carrier still adds only the identical V6 lock; neither V1 nor V6 is rewritten or requalified by this preparation. The fresh caller retains all 63 declarations, the accepted tighter 30-second per-test wrapper, original outer bounds and first-failure stop. No compilation, tests, guests or independent source approval have occurred on V2.

The earlier focused search guessed reverie/src/global_tool.rs and returned ENOENT/status 2. The corrected direct source is reverie/src/tool.rs:119–149. Both the actual trait and full readlink helper/existing caller were read; this was a path-read error, not an executed product failure.
''')
# Normalize the immutable failed predecessor's authenticated complete closure into the existing record schema.
external={}
oldindex=json.loads((F/'INPUTS.json').read_text())
for r in oldindex['files']:
    expected=dict(r,kind='file',file_mode=r['mode']);external[r['path']]=verify(expected)
for r in oldindex['symlinks']:external[r['path']]=verify(dict(r,kind='symlink'))
for r in oldindex['unexpanded_gitlinks']:external[r['path']]=verify(r)
for n in ['READBACK.json','INPUTS.json','REPORT.md','RESULTS.json','LEASE-RELEASE.json']:external[str(F/n)]=rec(F/n)
for r in current:external[r['path']]=r
for p in sorted(D.rglob('*')):
    if p.is_file()or p.is_symlink():external[str(p)]=rec(p)
write('INPUTS.json',dict(records=sorted(external.values(),key=lambda r:r['path']),scope='V2 unchanged production, exactly two test-source compile corrections; complete failed V1 source/evidence retained',historical_socket_metadata=oldindex['socket_metadata']))
write('TARGET.json',dict(status='SOURCE ONLY; V2 UNCOMPILED AND UNEXECUTED',source=str(D/'source'),source_entries=2626,base_target=rec(B/'TARGET.json'),v6_target=rec(V/'TARGET.json'),full_patch=rec(D/'SOURCE.patch'),delta=rec(D/'V1-TO-V2.patch'),manifest=rec(D/'SOURCE-MANIFEST.json'),continuity=rec(D/'SOURCE-CONTINUITY.json'),report=rec(D/'REPORT.md'),inputs=rec(D/'INPUTS.json'),selectors=rec(D/'SELECTORS.json'),only_two_test_source_corrections=True,production_identical_to_v1=True,planned_declarations=63))
for r in external.values():verify(r)
write('READBACK.json',dict(target=rec(D/'TARGET.json'),inputs=rec(D/'INPUTS.json'),source_manifest=rec(D/'SOURCE-MANIFEST.json'),all_records_unchanged=len(external),source_entries=2626,changed_v1_paths=changes,tests_or_build_executed=False,previous_failure_preserved=rec(F/'READBACK.json')))
print(json.dumps({n:rec(D/n)for n in ['TARGET.json','SOURCE.patch','V1-TO-V2.patch','INPUTS.json','READBACK.json']},indent=2))
