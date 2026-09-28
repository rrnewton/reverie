#!/usr/bin/python3
"""Prepare a source-bound phase; this program never runs Cargo or tests."""
import hashlib,json,os,re,subprocess,sys
from pathlib import Path
from common import bounded_process,check_file,file_record,json_read,read,require,write_new
HERE=Path(__file__).resolve().parent
REPO=HERE.parents[1]
TOOL=Path('/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin')

def scm():
    value={}
    for name,args in [('head',['rev-parse','HEAD']),('branch',['branch','--show-current']),('index',['ls-files','--stage'])]:
        raw=subprocess.check_output(['/usr/bin/git','-C',str(REPO),*args],timeout=5)
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

def main():
    name=sys.argv[1];kind='metadata'if name=='metadata'else'compile'if name=='compile'else'list'if name.startswith('list-')else'test'
    env={'HOME':'/home/newton','PATH':'/usr/local/bin:/usr/bin:/bin','CARGO_HOME':'/home/newton/.cargo','RUSTUP_HOME':'/home/newton/.rustup',
         'CARGO_TARGET_DIR':str(REPO/'target/process-alarm-qualification-v1'),'CARGO_BUILD_JOBS':'2','THIRD_PARTY_BUILD_JOBS':'2',
         'CARGO_NET_OFFLINE':'true','CARGO_NET_GIT_FETCH_WITH_CLI':'true','RUSTFLAGS':'','LC_ALL':'C','LANG':'C','TZ':'UTC',
         'CARGO_TERM_COLOR':'never','TMPDIR':str(HERE/'tmp'),'XDG_CACHE_HOME':str(HERE/'cache'),'GIT_OPTIONAL_LOCKS':'0','GIT_NO_LAZY_FETCH':'1',
         'USER':'newton','LOGNAME':'newton','XDG_RUNTIME_DIR':'/run/user/212630','DBUS_SESSION_BUS_ADDRESS':'unix:path=/run/user/212630/bus',
         'RUSTC':str(TOOL/'rustc'),'RUSTDOC':str(TOOL/'rustdoc'),'REVERIE_REQUIRE_KVM':'1'}
    selected=[];dependencies=[];artifact_selectors=[];links=[];inputs=[];absent=[]
    for parent in [REPO,*REPO.parents,Path('/home/newton')]:
        for rel in ['.cargo/config','.cargo/config.toml']:
            p=parent/rel
            if p.is_file():inputs.append(file_record(p.resolve()))
            else:absent.append(str(p))
    for p in [REPO/'Cargo.lock',HERE/'RUNNER_ORIGINS.json',HERE/'SELECTORS.json',*sorted(HERE.glob('*.py')),*sorted((HERE/'observer').glob('*.py')),HERE/'observer/source-inputs.json']:
        inputs.append(file_record(p))
    executables=[TOOL/'cargo',TOOL/'rustc',TOOL/'rustdoc',Path('/usr/bin/python3').resolve(),Path('/usr/bin/git'),Path('/usr/bin/gcc').resolve(),Path('/usr/bin/ld').resolve()]
    for i,p in enumerate(executables):
        record=file_record(p);inputs.append(record);extra,symlinks=loader('tool-'+str(i),record,env);inputs+=extra;links+=symlinks
    cargo=str(TOOL/'cargo')
    if kind=='metadata':argv=[cargo,'metadata','--offline','--locked','--format-version','1']
    elif kind=='compile':
        metadata=json_read(HERE/'controls/metadata/result.json');require(metadata['accepted']and metadata['terminal_authenticated'],'metadata did not qualify')
        require((HERE/'dependency-inputs.json').is_file(),'dependency sources not bound')
        inputs.append(file_record(HERE/'controls/metadata/result.json'))
        argv=[cargo,'test','--offline','--locked','-p','reverie-kvm','--lib','--test','static_elf','--no-run','--message-format=json']
        artifact_selectors=[dict(id='lib',target='reverie_kvm',kind=['lib']),dict(id='static',target='static_elf',kind=['test'])]
    else:
        qualified=json_read(HERE/'controls/compile/result.json');require(qualified['accepted']and qualified['terminal_authenticated'],'compile did not qualify')
        selection=json_read(HERE/'SELECTORS.json')
        artifact_id=name.removeprefix('list-')if kind=='list'else selection['groups'][name]['artifact']
        artifact=json_read(HERE/'controls/compile/artifacts.json')[artifact_id]['file'];check_file(artifact,executable=True)
        inputs += [file_record(HERE/'controls/compile/result.json'),file_record(HERE/'controls/compile/artifacts.json'),artifact]
        extra,symlinks=loader('test-'+artifact_id,artifact,env);inputs+=extra;links+=symlinks
        if kind=='list':
            selected=sorted({test for group in selection['groups'].values()if group['artifact']==artifact_id for test in group['names']})
            argv=[artifact['path'],'--list','-Z','unstable-options','--format=json']
        else:
            inventory_path=HERE/'controls'/('list-'+artifact_id)/'result.json';inventory=json_read(inventory_path)
            require(inventory['accepted']and inventory['terminal_authenticated'],'inventory did not qualify')
            inputs.append(file_record(inventory_path));selected=selection['groups'][name]['names']
            require(selected and all(inventory['readback']['names'].count(n)==1 for n in selected),'exact selected name missing or duplicated')
            argv=[artifact['path'],*selected,'--exact','--test-threads=1','--nocapture','-Z','unstable-options','--format=json']
    if(HERE/'dependency-symlinks.json').exists():
        inputs.append(file_record(HERE/'dependency-symlinks.json'));links+=json_read(HERE/'dependency-symlinks.json')
    target=REPO/'target/process-alarm-qualification-v1';t=target.stat();lease=HERE/'lane.lease';l=lease.stat();large=kind in('metadata','compile')
    plan=dict(path=str(HERE/(name+'-plan.json')),name=name,kind=kind,argv=argv,selected=selected,artifact_selectors=artifact_selectors,
              environment=env,scm=scm(),source_manifest=file_record(HERE/'source-manifest.json'),inputs=list({r['path']:r for r in inputs}.values()),
              input_symlinks=list({r['path']:r for r in links}.values()),absent_inputs=sorted(set(absent)),
              target=dict(path=str(target),identity=[t.st_dev,t.st_ino,t.st_uid]),
              lease=dict(path=str(lease),owner_slot=str(REPO),identity=[l.st_dev,l.st_ino,l.st_uid],state_directory=str(HERE/'lease-state')),
              output=str(HERE/'observer'/name),control=str(HERE/'controls'/name),
              limits=dict(aggregate_cpu_usec=600000000 if large else 30000000,wall_seconds=900 if large else 60,lethal_stderr_bytes=16777216,live_stdout_samples_bytes=67108864,phase_read_bytes=16777216,memory_bytes=17179869184,swap_bytes=0,free_floor_bytes=107374182400),
              scope='Unwired Reverie component only; no Hermit scheduler delivery, setitimer, periodic rearm or parity claim')
    if(HERE/'dependency-inputs.json').exists():plan['dependencies']=file_record(HERE/'dependency-inputs.json')
    write_new(Path(plan['path']),plan);print(plan['path'],hashlib.sha256(Path(plan['path']).read_bytes()).hexdigest())
if __name__=='__main__':main()
