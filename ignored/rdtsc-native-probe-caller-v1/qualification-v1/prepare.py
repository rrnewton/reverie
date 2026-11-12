#!/usr/bin/python3
"""Prepare one exact native plan; loader queries are bounded, never run the probe."""
import os
from pathlib import Path
import re
import sys
from common import bounded_process, check_file, file_record, json_read, read, require, write_new

HERE = Path(__file__).resolve().parent
OWNER = HERE.parent


def loader(label, executable, env):
    bound = HERE / 'loader' / (label + '.json')
    if not bound.exists():
        check_file(executable)
        q = bounded_process(['/usr/bin/ldd', executable['path']], OWNER, env,
                            HERE/'loader'/(label+'-ldd'), 5, 65536)
        r = bounded_process(['/usr/bin/readelf','-W','-l','-d',executable['path']], OWNER, env,
                            HERE/'loader'/(label+'-readelf'), 5, 65536)
        for query in [q,r]:
            require(query['returncode']==0 and not query['forced'], 'loader query failed')
            require(query['stdout']['bytes']<=65536 and query['stderr']['bytes']<=65536,
                    'loader query output exceeds exact bound, including fast exits')
        files, links = {}, {}
        for line in read(q['stdout']['path'],65536).decode().splitlines():
            require('not found' not in line, 'missing loader input')
            for spelling in re.findall(r'(/[^\s()]+)',line):
                p=Path(spelling)
                for a in [p,*p.parents]:
                    if a.is_symlink():links[str(a)]=dict(path=str(a),target=os.readlink(a),mode=a.lstat().st_mode&0o7777)
                resolved=p.resolve(strict=True);files[str(resolved)]=file_record(resolved)
        require(files,'empty loader closure')
        check_file(executable)
        write_new(bound,dict(executable=executable,inputs=list(files.values()),input_symlinks=list(links.values()),queries=[q,r],
                             query_programs=[file_record(Path(x)) for x in ['/usr/bin/ldd','/usr/bin/readelf']]))
    value=json_read(bound)
    require(value['executable']==executable,'loader executable changed')
    return [file_record(bound),*value['inputs'],*value['query_programs'],*[x[k] for x in value['queries'] for k in ['stdout','stderr']]], value['input_symlinks']


def main():
    require(len(sys.argv)==2,'expected one planned phase name')
    require(not (OWNER/'final-v1').exists(),'completed packet is frozen; no further phase')
    name=sys.argv[1]
    setup=json_read(HERE/'SETUP.json')
    selectors=json_read(OWNER/'SELECTORS.json')['cases']
    require(len(selectors)==42 and len({c['name'] for c in selectors})==42,'selector inventory differs')
    case=next((c for c in selectors if c['name']==name),None)
    require(name in ['dependencies','compile'] or case is not None,'unknown native phase')
    admission=json_read(HERE/'ADMISSION.json')
    check_file(admission['caller_manifest'])
    check_file(admission['setup'])
    require(admission['initially_empty'] and admission['shared_lane_lease_used'] is False,'fresh private admission missing')
    require(admission['target']['path']==setup['private_target'] and admission['lease']['path']==setup['private_lease'],'private identity differs')
    env={'HOME':'/home/newton','PATH':'/usr/bin:/bin','LC_ALL':'C','LANG':'C','TZ':'UTC',
         'TMPDIR':str(HERE/'tmp'),'USER':'newton','LOGNAME':'newton',
         'XDG_RUNTIME_DIR':'/run/user/'+str(os.getuid()),'DBUS_SESSION_BUS_ADDRESS':'unix:path=/run/user/'+str(os.getuid())+'/bus'}
    manifest=file_record(HERE/'CALLER-INPUTS.json')
    inputs=[manifest,file_record(HERE/'ADMISSION.json')]
    for row in json_read(manifest['path'])['records']:
        check_file(row);inputs.append(row)
    inputs+=setup['static_inputs']
    for row in setup['static_inputs']:
        check_file(row)
    links=list(setup['input_symlinks'])
    for row in links:
        alias=Path(row['path'])
        require(alias.is_symlink() and os.readlink(alias)==row['target']
                and (alias.lstat().st_mode & 0o7777)==row['mode'], 'tool/supervision alias changed before query')
    for index,spelling in enumerate(setup['tool_executables']):
        p=Path(spelling).resolve(strict=True);record=file_record(p)
        require(record in setup['static_inputs'],'tool is not in frozen compiler closure')
        extra,aliases=loader('tool-'+str(index),record,env);inputs+=extra;links+=aliases
    absent=list(setup['absent_inputs'])
    for name_absent in absent:
        p=Path(name_absent)
        require(not p.exists() and not p.is_symlink(),'unbound compiler specs appeared')
    kind='probe' if case else name
    source=setup['source']['path'];compiler=setup['compiler'];flags=setup['flags']
    kwargs={};missing=[]
    if name=='dependencies':
        argv=[compiler,*flags,'-M','-MT','probe',source]
    elif name=='compile':
        depresult=HERE/'controls/dependencies/result.json';prior=json_read(depresult)
        require(prior['accepted'] and prior['terminal_authenticated'] and prior['inputs_unchanged'],'dependency discovery did not qualify')
        deps=HERE/'HEADER-INPUTS.json';check_file(file_record(deps));bound=json_read(deps)
        require(bound['compiler_discovery_result']==file_record(depresult),'header binding has wrong discovery receipt')
        require(bound['source']==setup['source'],'header binding has wrong source')
        inputs += [file_record(depresult),file_record(deps),*bound['files']]
        links += bound['input_symlinks']
        output=HERE/'target/probe';depout=HERE/'target/probe.d'
        missing=[str(output),str(depout)]
        argv=[compiler,*flags,'-MD','-MF',str(depout),'-MT','probe','-Wl,-t',source,'-o',str(output)]
        kwargs=dict(expected_dependencies=bound['spelling_names'],dependency_output=str(depout),binary_output=str(output),
                    allowed_link_inputs=sorted({str(Path(p).resolve(strict=True)) for p in setup['link_inputs']}))
    else:
        prior_path=HERE/'controls/compile/result.json';prior=json_read(prior_path)
        require(prior['accepted'] and prior['terminal_authenticated'] and prior['inputs_unchanged'],'compile did not qualify')
        artifact_path=HERE/'retained-binaries/BINDING.json';binding=json_read(artifact_path)
        require(binding['compile_result']==file_record(prior_path),'retained binary belongs to another compile')
        artifact=binding['retained'];check_file(artifact,executable=True)
        inputs += [file_record(prior_path),file_record(artifact_path),artifact]
        extra,aliases=loader('native-probe',artifact,env);inputs+=extra;links+=aliases
        require(case['expected_exit']==0 and len(case['args'])==3,'selector altered')
        argv=[artifact['path'],*case['args']];kwargs=dict(probe_args=case['args'])
    for row in inputs:check_file(row)
    large=kind=='probe'
    limits=dict(aggregate_cpu_usec=setup['limits']['probe_cpu_usec'] if large else setup['limits']['compiler_cpu_usec'],
                wall_seconds=setup['limits']['probe_wall_seconds'] if large else setup['limits']['compiler_wall_seconds'],
                **{k:v for k,v in setup['limits'].items() if k not in ['probe_cpu_usec','compiler_cpu_usec','probe_wall_seconds','compiler_wall_seconds']})
    path=HERE/'plans'/(name+'.json')
    plan=dict(path=str(path),name=name,kind=kind,owner=str(OWNER),cwd=str(OWNER),argv=argv,environment=env,
              inputs=list({r['path']:r for r in inputs}.values()),input_symlinks=list({r['path']:r for r in links}.values()),
              absent_inputs=absent,target=admission['target'],lease=admission['lease'],must_be_absent_before=missing,
              output=str(HERE/'observer'/name),control=str(HERE/'controls'/name),limits=limits,**kwargs)
    write_new(path,plan)
    print(str(path),file_record(path)['sha256'])

if __name__=='__main__':
    main()
