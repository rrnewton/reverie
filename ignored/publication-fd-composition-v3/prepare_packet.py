"""Source/evidence preparation only; never runs a product, Cargo, or a lease."""
from pathlib import Path
import difflib, hashlib, json, os, re, shutil, stat, subprocess
D=Path(__file__).resolve().parent;R=D.parents[1];V2=D.parent/'publication-fd-composition-v2'
Q=D.parent/'publication-fd-composition-qualification-v1/qualification-v1';F=Q.parent/'final-v1'
S=R.parent/'kvm-parent-reader-support-20260916'
paths=['reverie-kvm/src/elf.rs','reverie-kvm/src/executor.rs','reverie-kvm/src/process_signal_publication.rs']
base='000c15a1161ea2d58749431b5ddaaa97f7aa37d5'
added=['inherited_stdin_epoll_preserves_local_and_authoritative_handles','inherited_stdin_sibling_replacement_preserves_old_dup','inherited_stdin_fork_exec_and_exit_preserve_entry_lifetime']
def record(p):
 b=p.read_bytes();return dict(path=str(p),bytes=len(b),mode=stat.S_IMODE(p.stat().st_mode),sha256=hashlib.sha256(b).hexdigest())
def put(n,v):
 p=D/n;p.parent.mkdir(parents=True,exist_ok=True)
 data=json.dumps(v,indent=2)+'\n'
 if p.exists():assert p.read_text()==data,p
 else:p.write_text(data)
def copy(a,b):
 if b.exists():assert a.read_bytes()==b.read_bytes(),b
 else:b.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(a,b)
def patch(a,b,rels):
 out=''
 for rel in rels:
  pa=a/rel;sa=pa.read_text() if pa.is_file() else '';sb=(b/rel).read_text()
  out+=''.join(difflib.unified_diff(sa.splitlines(keepends=True),sb.splitlines(keepends=True),fromfile='a/'+rel if sa else '/dev/null',tofile='b/'+rel))
 return out
reviews=[S/'ignored/kvm-publisher-fd-native-review-v2-20260918/01-source.final.txt',S/'ignored/kvm-stdin-retirement-control-review-20260918/REPORT.md']
assert record(reviews[0])['sha256']=='3144dd5c4cd68741e7ff3bdd339340ab8c9b2022a7d409e425e1784fda850bea'
assert record(reviews[1])['sha256']=='d1779c6c31d7175ab8e79cbb3060462b0dc78d966d69e8cd1aca05675a308bca'
for i,p in enumerate(reviews,1):copy(p,D/'review-requirements'/f'{i:02}.txt')
for rel in paths:
 copy(V2/'source'/rel,D/'before'/rel);copy(D/'source'/rel,D/'after'/rel)
 if (V2/'base'/rel).exists():copy(V2/'base'/rel,D/'base'/rel)
(D/'SOURCE.patch').write_text(patch(D/'base',D/'source',paths))
(D/'V2-TO-V3.patch').write_text(patch(V2/'source',D/'source',paths))
assert (D/'source'/paths[2]).read_bytes()==(V2/'source'/paths[2]).read_bytes()
ex=(D/'source'/paths[1]).read_text();oldex=(V2/'source'/paths[1]).read_text()
start=ex.index('    fn inherited_pipe_stdin_fixture');end=ex.index('    type RetirementObservations',start)
block=ex[start:end];(D/'NEW-TESTS.rs').write_text(block)
if not (D/'baseline-source').exists():shutil.copytree(V2/'source',D/'baseline-source',symlinks=True)
baseline_ex=oldex.replace('    type RetirementObservations',block+'    type RetirementObservations',1)
(D/'baseline-source'/paths[1]).write_text(baseline_ex)
assert baseline_ex.replace(block,'',1)==oldex
(D/'BASELINE-TEST.patch').write_text(patch(V2/'source',D/'baseline-source',paths))
put('BASELINE-CONTINUITY.json',dict(production_and_old_tests_byte_identical_to_v2=True,only_addition=record(D/'NEW-TESTS.rs'),test_patch=record(D/'BASELINE-TEST.patch'),new_bodies_identical_between_baseline_and_corrected=True,no_baseline_only_production_adapter=True,execution_performed=False))
# Rust functions in these named tests have simple balanced braces; strings and
# comments are treated atomically so assertion comparisons do not erase text.
tokens=re.compile(r'//[^\n]*|/\*.*?\*/|(?:br|r)(?P<h>\#*)".*?"(?P=h)|(?:b)?"(?:\\.|[^"\\])*"|\s+|[A-Za-z_][A-Za-z_0-9]*|[^\s]',re.S)
def tok(s):return [m.group() for m in tokens.finditer(s) if not m.group().isspace() and not m.group().startswith(('//','/*'))]
def fn(s,name):
 match=re.search(r'\bfn\s+'+re.escape(name)+r'\s*\(',s);assert match,name
 a=match.start();t=tok(s[a:]);i=t.index('{');depth=1;j=i+1
 while depth:
  depth+=(t[j]=='{')-(t[j]=='}');j+=1
 return t[:j]
def assertions(t):
 out=[];i=0
 while i<len(t)-2:
  if t[i] in ['assert','assert_eq','assert_ne'] and t[i+1]=='!':
   start=i;i+=3;depth=1
   while depth:depth+=(t[i]=='(')-(t[i]==')');i+=1
   out.append(t[start:i])
  else:i+=1
 return out
selection=json.loads((Q/'SELECTORS.json').read_text());old_groups=selection['groups'];all_old=[n for g in old_groups.values() for n in g['names']];assert len(all_old)==len(set(all_old))==78
continuity={};changed=[]
for name in all_old:
 if name.startswith('executor::tests::'):
  short=name.split('::')[-1]
  if re.search(r'\bfn\s+'+re.escape(short)+r'\s*\(',oldex):a=fn(oldex,short);b=fn(ex,short)
  else:
   found=[]
   for included in ['pipe_fionread_tests.rs','child_exit_signal_tests.rs','process_alarm_signal_tests.rs','signal_dequeue_tests.rs']:
    oldtext=(V2/'source/reverie-kvm/src'/included).read_text();newtext=(D/'source/reverie-kvm/src'/included).read_text()
    if re.search(r'\bfn\s+'+re.escape(short)+r'\s*\(',oldtext):
     assert oldtext==newtext;found.append(included);a=fn(oldtext,short);b=fn(newtext,short)
   assert len(found)==1,(name,found)
  if a!=b:
   changed.append(name);aa=assertions(a);bb=assertions(b);i=0
   for assertion in bb:
    if i<len(aa) and assertion==aa[i]:i+=1
   assert i==len(aa),(name,'lost assertion',i,len(aa))
   continuity[name]=dict(old_assertions=len(aa),new_assertions=len(bb),old_assertions_retained_in_order=True)
assert changed==['executor::tests::descriptor_retirement_install_error_releases_both_guards','executor::tests::descriptor_retirement_accept_cleanup_releases_both_guards'],changed
# The comment-only EMFILE edit tokenizes identically; all other existing test
# source outside the additive block/helper/setup changes is checked by this diff.
put('TEST-CONTINUITY.json',dict(original_declarations=78,added_declarations=added,changed_old_test_token_bodies=changed,assertions=continuity,all_other_selected_test_token_bodies_unchanged=True,real_emfile_body_identical_except_comment=True,static_and_publisher_sources_identical=True,original_selector_groups_identical=True,not_execution=True))
old_manifest=json.loads((F/'FULL-SOURCE-MANIFEST.json').read_text())
def manifest(root,filename):
 rows=[];qualification=[]
 for original in old_manifest:
  rel=original['relative'];p=root/rel;row={k:original[k] for k in ['relative','mode','kind','publisher_git_object'] if k in original}
  if row['kind']=='file':
   row.update(record(p));row['file_mode']=row.pop('mode');row['mode']=original['mode']
   if rel not in paths:assert row['sha256']==original['sha256'],rel
  elif row['kind']=='symlink':
   row.update(target=os.readlink(p),sha256=hashlib.sha256(os.readlink(p).encode()).hexdigest());assert row['sha256']==original['sha256']
  else:assert p.is_dir() and not list(p.iterdir())
  rows.append(row)
  qualification.append(dict(path=rel,mode=row['mode'],**({'git_object':row['publisher_git_object']} if row['kind']=='unexpanded_gitlink' else {'sha256':row['sha256']})))
 put(filename,rows);return qualification
corrected_manifest=manifest(D/'source','SOURCE-MANIFEST.json');baseline_manifest=manifest(D/'baseline-source','BASELINE-SOURCE-MANIFEST.json')
for root in [D/'source',D/'baseline-source']:assert record(root/'Cargo.lock')['sha256']=='1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856'
selection['groups']=dict(old_groups)
for i,name in enumerate(added,1):selection['groups'][f'stdin-{i:02}']=dict(artifact='lib',names=['executor::tests::'+name])
selection.update(proposed_unique_declarations=81,proposed_lib_declarations=72,proposed_static_declarations=9,execution_performed=False,added_declarations=['executor::tests::'+n for n in added])
assert all(selection['groups'][k]==v for k,v in old_groups.items())
baseline_selection=dict(groups={'baseline-neighbor':old_groups['fd-04'],'baseline-stdin':selection['groups']['stdin-01']},proposed_unique_declarations=2,proposed_lib_declarations=2,proposed_static_declarations=0,execution_performed=False)
for directory,root,groups,source_manifest in [('qualification-proposal',D/'source',selection,corrected_manifest),('baseline-proposal',D/'baseline-source',baseline_selection,baseline_manifest)]:
 for rel in paths:copy(root/rel,D/directory/'after'/rel)
 put(directory+'/SETUP.json',dict(owner_slot=str(R),source_root=str(root),base=base,source_files=[dict(relative=rel,file=record(D/directory/'after'/rel)) for rel in paths],purpose='Unexecuted inherited stdin identity before/after proposal; no activation'))
 put(directory+'/SELECTORS.json',groups);put(directory+'/source-manifest.json',source_manifest);copy(Q/'prepare.py',D/directory/'prepare.py')
helpers=[]
for p in sorted([*Q.glob('*.py'),*(Q/'observer').glob('*.py'),Q/'observer/source-inputs.json']):
 rel=p.relative_to(Q);copy(p,D/'caller'/rel);helpers.append(dict(qualified_original=record(p),proposed=record(D/'caller'/rel),byte_identical=True))
put('CALLER_INPUTS.json',dict(helpers=helpers,prepare_byte_identical_to_qualified=True,logic_delta='',changed_data_only=['SETUP.json','source-manifest.json','SELECTORS.json'],baseline_proposal=[record(p) for p in sorted((D/'baseline-proposal').glob('*')) if p.is_file()],corrected_proposal=[record(p) for p in sorted((D/'qualification-proposal').glob('*')) if p.is_file()],not_deployed=True,execution_performed=False))
put('SOURCE_INPUTS.json',dict(base=base,predecessor_target=record(V2/'TARGET.json'),predecessor_qualification_target=record(F/'TARGET.json'),predecessor_patch=record(V2/'SOURCE.patch'),requirements=[record(p) for p in reviews],source_manifest=record(D/'SOURCE-MANIFEST.json'),after=[dict(relative=p,file=record(D/'source'/p)) for p in paths],actual_ignored_lock=record(D/'source/Cargo.lock'),production_execution_performed=False,source_formatter_only=record(D/'SOURCE-FORMAT.json')))
hold=json.loads((F/'SOURCE_INPUTS.json').read_text())['live_state_unchanged']
env=dict(os.environ,GIT_OPTIONAL_LOCKS='0')
def git(*args):
 p=subprocess.run(['/usr/bin/git','-C',str(R),*args],capture_output=True,timeout=30,env=env);assert p.returncode==0,p.stderr;return p.stdout
assert git('rev-parse','HEAD').decode().strip()==hold['head']==base
assert git('branch','--show-current').decode().strip()==hold['branch']
assert not git('diff','--cached','--name-only')
assert record(Path(hold['index']['path']))['sha256']==hold['index']['sha256']
for row in hold['live']:assert record(Path(row['path']))['sha256']==row['sha256']
put('LIVE-CONTINUITY.json',dict(live_source_head_index_unchanged=True,head=hold['head'],branch=hold['branch'],index=record(Path(hold['index']['path'])),live=[record(Path(x['path'])) for x in hold['live']]))
print('prepared; source-only, not qualified')
