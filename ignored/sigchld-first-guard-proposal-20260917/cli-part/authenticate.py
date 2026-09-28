from pathlib import Path
import hashlib,json,re,subprocess,stat
P=Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-proc-fd-identity-20260917/ignored/sigchld-first-guard-proposal-20260917/cli-part')
base=json.loads((P/'BASE.json').read_text());files=json.loads((P/'FILES.json').read_text());diff=(P/'candidate.patch').read_text().splitlines(keepends=True)
checks=[];i=0;applied=[]
while i<len(diff):
 assert diff[i].startswith('diff --git ')
 name=diff[i].split(' b/',1)[1].strip();i+=1
 assert diff[i]=='--- a/'+name+'\n';i+=1
 assert diff[i]=='+++ b/'+name+'\n';i+=1
 source=(P/'base'/name).read_text().splitlines(keepends=True);dest=[];cursor=0
 while i<len(diff) and not diff[i].startswith('diff --git '):
  h=re.fullmatch(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\n',diff[i]);assert h,diff[i]
  oldstart=int(h[1])-1;oldcount=int(h[2] or '1');newcount=int(h[4] or '1');assert oldstart>=cursor
  dest+=source[cursor:oldstart];cursor=oldstart;i+=1;oldseen=newseen=0
  while i<len(diff) and not diff[i].startswith(('@@ ','diff --git ')):
   line=diff[i];tag=line[0];body=line[1:]
   if tag in (' ','-'):assert source[cursor]==body;cursor+=1;oldseen+=1
   if tag in (' ','+'):dest.append(body);newseen+=1
   assert tag in (' ','+','-');i+=1
  assert (oldseen,newseen)==(oldcount,newcount)
 dest+=source[cursor:];expected=(P/'changed'/name).read_text();assert ''.join(dest)==expected
 applied.append(name)
for f in files:
 a=(P/'base'/f['path']).read_bytes();b=(P/'changed'/f['path']).read_bytes()
 assert hashlib.sha256(a).hexdigest()==f['base_sha256'] and hashlib.sha256(b).hexdigest()==f['sha256']
 assert len(a)==f['base_bytes'] and len(b)==f['bytes'] and stat.S_IMODE((P/'changed'/f['path']).stat().st_mode)==0o644
 mode=subprocess.run(['git','ls-tree',base['base'],'--',f['path']],cwd=base['repository'],capture_output=True,text=True,timeout=10,check=True).stdout
 assert mode.startswith('100644 blob ')
 old=set(re.findall(r'#\[(?:tokio::)?test[^\]]*\]\s*(?:async\s+)?fn (\w+)',a.decode()))
 new=set(re.findall(r'#\[(?:tokio::)?test[^\]]*\]\s*(?:async\s+)?fn (\w+)',b.decode()))
 assert old<=new
 checks.append(dict(path=f['path'],base_mode='100644',preview_mode='100644',all_prior_source_test_names_retained=True,added_source_test_names=sorted(new-old)))
assert sorted(applied)==sorted(f['path'] for f in files)
assert not any(x.startswith('-') and not x.startswith('---') and ('assert!' in x or 'assert_eq!' in x or 'assert_ne!' in x or '#[ignore' in x) for x in diff)
result=dict(base=base['base'],complete_patch_sha256=hashlib.sha256((P/'candidate.patch').read_bytes()).hexdigest(),exact_three_file_in_memory_patch_application=True,source_checks=checks,no_removed_assertion_lines=True,verification_scope='Source/patch/file-mode readback only. No Rust compiler, formatter, tests, runtime, guest, signal or network operation.')
with (P/'READBACK.json').open('x') as f:json.dump(result,f,indent=2);f.write('\n')
print(json.dumps(result,indent=2))
