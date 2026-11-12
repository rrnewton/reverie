#!/usr/bin/python3
"""Prepare a source-bound phase; this program never runs Cargo or tests."""
import hashlib,json,os,re,subprocess,sys
from pathlib import Path
from common import bounded_process,check_file,file_record,json_read,read,require,write_new
HERE=Path(__file__).resolve().parent
OWNER = HERE.parents[2]
REPO = Path(json_read(HERE/'SETUP.json')['source_root'])
require(REPO == OWNER or REPO.is_relative_to(OWNER/'ignored'), 'source root outside owned slot')
TOOL=Path('/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin')

def scm():
    value={}
    for name,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
        raw=subprocess.check_output(['/usr/bin/git','-C',str(OWNER),*args],timeout=30)
        value[name]=hashlib.sha256(raw).hexdigest()if name=='index'else raw.decode().strip()
    return value

def loader(label,executable,env):
    bound=HERE/'loader'/(label+'.json')
    if not bound.exists():
        q=bounded_process(['/usr/bin/ldd',executable['path']],REPO,env,HERE/'loader'/(label+'-ldd'),5,65536)
        r=bounded_process(['/usr/bin/readelf','-W','-l','-d',executable['path']],REPO,env,HERE/'loader'/(label+'-readelf'),5,65536)
        require(q['returncode']==r['returncode']==0 and not(q['forced']or r['forced']),'loader/ELF query failed')
        files={};links={}
        for line in read(q['stdout']['path']).decode().splitlines():
            require('not found'not in line,'missing loader input')
            for spelling in re.findall(r'(/[^\s()]+)',line):
                p=Path(spelling)
                for ancestor in [p,*p.parents]:
                    if ancestor.is_symlink():links[str(ancestor)]=dict(path=str(ancestor),target=os.readlink(ancestor))
                resolved=p.resolve(strict=True);files[str(resolved)]=file_record(resolved)
        require(files,'empty loader closure')
        write_new(bound,dict(executable=executable,inputs=list(files.values()),input_symlinks=list(links.values()),queries=[q,r],query_programs=[file_record(Path(x))for x in ['/usr/bin/ldd','/usr/bin/readelf']]))
    value=json_read(bound);require(value['executable']==executable,'loader executable changed')
    return [file_record(bound),*value['inputs'],*value['query_programs'],*[q[k]for q in value['queries']for k in ['stdout','stderr']]],value['input_symlinks']

