from pathlib import Path
import hashlib,json,re,subprocess,stat
D=Path(__file__).resolve().parent;O=D/'composition-v10-5bc8';O.mkdir(exist_ok=False)
R='/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917';OLD='8051335e87104f7cf832204f74920d38416393b2';BASE='5bc8dfd9a2c024426aaefe504f250a71394bbb09'
h=lambda b:hashlib.sha256(b).hexdigest()
def blobid(b):return hashlib.sha1(b'blob '+str(len(b)).encode()+b'\0'+b).hexdigest()
def git(*args):return subprocess.check_output(['git',*args],cwd=R,timeout=20)
def bind(p):
 p=Path(p);b=p.read_bytes();return {'path':str(p),'bytes':len(b),'sha256':h(b)}
def write(name,obj):
 data=obj if isinstance(obj,bytes) else (json.dumps(obj,indent=2)+'\n').encode()
 with (O/name).open('xb') as f:f.write(data)
packet=json.loads((D/'PACKET-v10.json').read_text())
for row in packet['inputs']:assert bind(row['path'])==row
manifest=json.loads((D/'PREVIEW-MANIFEST-v10.json').read_text());changed=[row for row in manifest['files'] if row['changed']];unchanged=[row for row in manifest['files'] if not row['changed']]
for row in manifest['files']:
 b=Path(row['preview_path']).read_bytes();assert len(b)==row['bytes'] and h(b)==row['sha256'];assert oct(stat.S_IMODE(Path(row['preview_path']).stat().st_mode))==row['mode']
def tree(rev):
 rows={}
 for raw in git('ls-tree','-r','-z',rev).split(b'\0'):
  if not raw:continue
  header,path=raw.split(b'\t',1);mode,kind,oid=header.decode().split();rows[path.decode()]={'path':path.decode(),'mode':mode,'kind':kind,'object':oid}
 return rows
oldtree=tree(OLD);basetree=tree(BASE);candidate=dict(basetree)
incoming=sorted(p for p in set(oldtree)|set(basetree) if oldtree.get(p)!=basetree.get(p));changed_names={r['path'] for r in changed};overlap=sorted(changed_names.intersection(incoming));assert not overlap,overlap
inputs={}
for row in changed:
 path=row['path']
 assert oldtree.get(path)==basetree.get(path),path
 assert row['expected_git_mode']==(basetree[path]['mode'] if path in basetree else '100644')
 assert int(row['mode'],8)==int(row['expected_git_mode'][-3:],8)
 data=git('show',BASE+':'+path) if path in basetree else b''
 inputs[path]=data
patch=(D/'candidate-v10.patch').read_bytes();lines=patch.decode().splitlines(True);i=0;outputs={};hunks=[]
while i<len(lines):
 assert lines[i].startswith('--- ');oldname=lines[i][4:].rstrip('\n');i+=1
 assert lines[i].startswith('+++ b/');path=lines[i][6:].rstrip('\n');i+=1
 assert path in inputs and oldname in ('/dev/null','a/'+path)
 if oldname=='/dev/null':assert path not in basetree
 original=inputs[path].decode().splitlines(True);cursor=0;result=[]
 while i<len(lines) and not lines[i].startswith('--- '):
  m=re.fullmatch(r'@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@\n',lines[i]);assert m,(path,i,lines[i]);i+=1
  on=int(m[2] or 1);nn=int(m[4] or 1);a=int(m[1])-(1 if on else 0);b=int(m[3])-(1 if nn else 0)
  assert a>=cursor;result+=original[cursor:a];cursor=a;assert len(result)==b
  seen_old=seen_new=0
  while i<len(lines) and not lines[i].startswith(('@@ ','--- ')):
   line=lines[i];i+=1;assert line[:1] in (' ','-','+')
   if line[0] in (' ','-'):assert cursor<len(original) and original[cursor]==line[1:],(path,cursor,line);cursor+=1;seen_old+=1
   if line[0] in (' ','+'):result.append(line[1:]);seen_new+=1
  assert (on,nn)==(seen_old,seen_new)
  hunks.append({'path':path,'base_start':int(m[1]),'base_lines':on,'output_start':int(m[3]),'output_lines':nn,'exact_context_and_positions':True})
 result+=original[cursor:];outputs[path]=''.join(result).encode()
assert set(outputs)==changed_names
file_rows=[]
for row in changed:
 path=row['path'];data=outputs[path];assert data==Path(row['preview_path']).read_bytes()
 p=O/'changed'/path;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(data)
 mode=basetree[path]['mode'] if path in basetree else '100644';p.chmod(int(mode[-3:],8))
 candidate[path]={'path':path,'mode':mode,'kind':'blob','object':blobid(data)}
 file_rows.append({'path':path,'base_entry':basetree.get(path),'base_sha256':h(inputs[path]) if path in basetree else None,'output_path':str(p),'mode':mode,'bytes':len(data),'sha256':h(data),'git_blob':blobid(data),'preview_v10_sha256':row['sha256']})
assert all(candidate[p]==row for p,row in basetree.items() if p not in changed_names)
# Preserve exact immutable upstream changes and explicitly important unchanged inputs.
preserve_paths=sorted(set(incoming)|{'Cargo.toml','rust-toolchain.toml','ci/configure-build-jobs.sh'})
preserved=[]
for path in preserve_paths:
 assert path in basetree and basetree[path]['kind']=='blob'
 data=git('show',BASE+':'+path);p=O/'base-inputs'/path;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(data)
 if path not in changed_names:assert candidate[path]==basetree[path]
 preserved.append({'path':path,'base_entry':basetree[path],'copy':bind(p),'incoming_change':path in incoming,'unchanged_in_composition':path not in changed_names,'comment_only_candidate':path=='ci/configure-build-jobs.sh'})
# Git tree IDs are calculated from in-memory entries; this writes no Git objects.
def tree_id(entries):
 root={}
 for path,row in entries.items():
  parts=path.split('/');at=root
  for part in parts[:-1]:at=at.setdefault(part,{})
  at[parts[-1]]=(row['mode'],row['object'])
 def digest(node):
  parts=[]
  for name,item in node.items():
   directory=isinstance(item,dict);mode,oid=('40000',digest(item)) if directory else item
   parts.append((name.encode()+(b'/' if directory else b''),mode.encode()+b' '+name.encode()+b'\0'+bytes.fromhex(oid)))
  content=b''.join(value for _,value in sorted(parts));return hashlib.sha1(b'tree '+str(len(content)).encode()+b'\0'+content).hexdigest()
 return digest(root)
assert tree_id(basetree)==git('rev-parse',BASE+'^{tree}').decode().strip()
write('5bc8-to-m2-v10.patch',patch)
write('BASE-TREE.json',{'commit':BASE,'tree':tree_id(basetree),'entries':[basetree[p] for p in sorted(basetree)]})
write('EXPECTED-TREE.json',{'scope':'in-memory expected source tree; not a commit, object-store write, compiled identity or execution','base_commit':BASE,'expected_tree':tree_id(candidate),'entries':[candidate[p] for p in sorted(candidate)]})
write('FILES.json',{'base_commit':BASE,'source_patch':bind(D/'candidate-v10.patch'),'files':file_rows})
write('PRESERVED-INPUTS.json',{'base_commit':BASE,'files':preserved,'five_unchanged805_preview_files_not_copied':[r['path'] for r in unchanged]})
write('READBACK.json',{'method':'exact unified diff hunks applied to immutable 5bc Git blobs in memory; no fuzz, replacement of surrounding files, index/ref/worktree writes or product execution','original_preview_base':OLD,'composition_base':BASE,'base_tree':tree_id(basetree),'expected_composed_tree':tree_id(candidate),'base_tree_entries':len(basetree),'expected_tree_entries':len(candidate),'changed_paths':sorted(changed_names),'incoming_805_to_5bc_paths':incoming,'overlapping_changed_paths':overlap,'all_unmodified_tree_entries_preserved':True,'all33_composed_files_equal_v10_changed_previews':True,'five_unchanged805_preview_files_not_copied':[r['path'] for r in unchanged],'hunks':hunks})
write('805-to-5bc-integration.patch',git('diff','--binary',OLD,BASE))
print(json.dumps({'composition_dir':str(O),'readback':bind(O/'READBACK.json'),'files':bind(O/'FILES.json'),'patch':bind(O/'5bc8-to-m2-v10.patch'),'base_tree':tree_id(basetree),'expected_tree':tree_id(candidate),'base_entries':len(basetree),'composed_entries':len(candidate),'incoming_paths':incoming,'overlap':overlap},indent=2))
