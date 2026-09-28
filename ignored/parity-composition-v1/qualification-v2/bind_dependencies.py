#!/usr/bin/python3
"""Bind actual metadata-resolved dependency sources; no compilation or execution."""
import json,os
from pathlib import Path
from common import file_record,json_read,require,write_new
HERE=Path(__file__).resolve().parent
REPO=Path(json_read(HERE/'SETUP.json')['source_root'])
result=json_read(HERE/'controls/metadata/result.json')
require(result['accepted']and result['terminal_authenticated']and result['raw_status']==0,'metadata not qualified')
metadata=json_read(HERE/'observer/metadata/stdout')
packages={p['id']:p for p in metadata['packages']}
nodes={n['id']:n for n in metadata['resolve']['nodes']}
roots=[p['id']for p in metadata['packages']if p['manifest_path']==str(REPO/'reverie-kvm/Cargo.toml')]
require(len(roots)==1,'KVM root absent or ambiguous')
seen=set();pending=list(roots)
while pending:
    key=pending.pop()
    if key in seen:continue
    seen.add(key);pending.extend(dep['pkg']for dep in nodes[key]['deps'])
files={};links={};closure=[]
for key in sorted(seen):
    package=packages[key];directory=Path(package['manifest_path']).parent.resolve(strict=True)
    closure.append(dict(id=key,manifest=package['manifest_path'],source=package['source'],directory=str(directory)))
    if directory.is_relative_to(REPO):continue
    for path in sorted(directory.rglob('*')):
        if path.is_symlink():
            links[str(path)]=dict(path=str(path),target=os.readlink(path))
            require(not path.is_dir(),'dependency directory symlink needs explicit traversal')
            path=path.resolve(strict=True)
        if path.is_file():files[str(path)]=file_record(path)
require(files,'external dependency closure empty')
write_new(HERE/'dependency-inputs.json',list(files.values()))
write_new(HERE/'dependency-symlinks.json',list(links.values()))
write_new(HERE/'dependency-closure.json',dict(metadata=file_record(HERE/'observer/metadata/stdout'),packages=closure,file_count=len(files),bytes=sum(x['bytes']for x in files.values())))
print(json.dumps(dict(packages=len(seen),external_files=len(files),external_bytes=sum(x['bytes']for x in files.values()))))
