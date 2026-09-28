#!/usr/bin/python3
"""Bind qualified metadata's ptrace dependency source closure; no build or test."""
import json,os
from pathlib import Path
from common import check_file,file_record,json_read,require,write_new
HERE=Path(__file__).resolve().parent
REPO=Path(json_read(HERE/'SETUP.json')['source_root'])
META=HERE
result=json_read(META/'controls/metadata/result.json')
require(result['accepted'] and result['terminal_authenticated'] and result['inputs_unchanged'] and result['raw_status']==0,'metadata not qualified')
metadata=json_read(META/'observer/metadata/stdout')
require(metadata['workspace_root']==str(REPO),'metadata source differs')
packages={p['id']:p for p in metadata['packages']};nodes={n['id']:n for n in metadata['resolve']['nodes']}
roots=[p['id'] for p in packages.values() if p['name']=='reverie-ptrace' and p['manifest_path']==str(REPO/'reverie-ptrace/Cargo.toml') and p['source'] is None]
require(len(roots)==1,'ptrace metadata root absent or ambiguous')
seen=set();pending=list(roots)
while pending:
 key=pending.pop()
 if key in seen:continue
 seen.add(key);pending.extend(dep['pkg'] for dep in nodes[key]['deps'])
files={};links={};closure=[]
for key in sorted(seen):
 package=packages[key];directory=Path(package['manifest_path']).parent.resolve(strict=True)
 closure.append(dict(id=key,manifest=package['manifest_path'],source=package['source'],directory=str(directory)))
 if directory.is_relative_to(REPO):continue
 require(package['source'] is not None,'unbound external path package')
 for path in sorted(directory.rglob('*')):
  if path.is_symlink():
   links[str(path)]=dict(path=str(path),target=os.readlink(path));require(not path.is_dir(),'dependency directory alias requires explicit traversal');path=path.resolve(strict=True)
  if path.is_file():files[str(path)]=file_record(path)
require(files,'external dependency closure empty')
write_new(HERE/'dependency-inputs.json',list(files.values()))
write_new(HERE/'dependency-symlinks.json',list(links.values()))
write_new(HERE/'dependency-closure.json',dict(metadata=file_record(META/'observer/metadata/stdout'),qualified_result=file_record(META/'controls/metadata/result.json'),roots=roots,packages=closure,file_count=len(files),bytes=sum(x['bytes'] for x in files.values())))
print(json.dumps(dict(packages=len(seen),external_files=len(files),external_bytes=sum(x['bytes'] for x in files.values()))))
