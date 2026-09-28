#!/usr/bin/python3
"""Prepare a fixed phase after authenticating its predecessor and exact ELF closure."""
import json,os,re,sys
from pathlib import Path
from common import bounded_process,check_file,digest,file_record,json_read,read,require,write_new
HERE=Path(__file__).resolve().parent
REPO=Path(json_read(HERE/'SETUP.json')['source_root'])

def unique(rows):
 result={}
 for row in rows:
  require(row['path'] not in result or result[row['path']]==row,'conflicting input identity')
  result[row['path']]=row
 return list(result.values())

def loader(key,executable,env):
 bound=HERE/'loader'/(key+'.json')
 if not bound.exists():
  original=json_read(HERE/'metadata-plan.json');programs=[]
  for name in ['/usr/bin/ldd','/usr/bin/readelf']:
   matches=[x for x in original['inputs'] if x['path']==name]
   require(len(matches)==1,'loader query tool not bound');check_file(matches[0]);programs.append(matches[0])
  queries=[]
  for label,argv in [('ldd',['/usr/bin/ldd',executable['path']]),('readelf',['/usr/bin/readelf','-W','-l','-d',executable['path']])]:
   q=bounded_process(argv,REPO,env,HERE/'loader'/(key+'-'+label),5,65536)
   require(q['returncode']==0 and not q['forced'],'loader query failed')
   require(q['stdout']['bytes']<=65536 and q['stderr']['bytes']<=65536,'loader query stream bound');queries.append(q)
  files={};links={}
  for line in read(queries[0]['stdout']['path'],65536).decode().splitlines():
   require('not found' not in line,'missing linked library')
   for spelling in re.findall(r'(/[^\s()]+)',line):
    p=Path(spelling)
    for ancestor in [p,*p.parents]:
     if ancestor.is_symlink():links[str(ancestor)]=dict(path=str(ancestor),target=os.readlink(ancestor))
    p=p.resolve(strict=True);files[str(p)]=file_record(p)
  require(files,'empty loader closure')
  for program in programs:check_file(program)
  check_file(executable,executable=True)
  write_new(bound,dict(executable=executable,inputs=list(files.values()),input_symlinks=list(links.values()),queries=queries,query_programs=programs))
 value=json_read(bound);require(value['executable']==executable,'loader executable changed')
 for row in value['inputs']:check_file(row)
 return [file_record(bound),*value['inputs'],*value['query_programs'],*[q[k] for q in value['queries'] for k in ['stdout','stderr']]],value['input_symlinks']

def main():
 require(len(sys.argv)==2,'one exact phase required')
 name=sys.argv[1];rows=[p for p in json_read(HERE/'PHASES.json')['phases'] if p['name']==name];require(len(rows)==1,'unknown phase');row=rows[0]
 if name=='metadata':
  print(HERE/'metadata-plan.json',digest(HERE/'metadata-plan.json'));return
 meta=json_read(HERE/'controls/metadata/result.json')
 require(meta['accepted'] and meta['terminal_authenticated'] and meta['inputs_unchanged'] and meta['raw_status']==0,'metadata not qualified')
 plan=json_read(HERE/'metadata-plan.json')
 plan.update(name=name,kind=row['kind'],path=str(HERE/(name+'-plan.json')),argv=row['argv'],selected=row['selected'],artifact_selectors=row['artifact_selectors'],output=str(HERE/'observer'/name),control=str(HERE/'controls'/name),scope='Fresh isolated clock and ptrace injection correction; no KVM or Hermit parity credit')
 plan['limits']=row['limits']
 plan['inputs'] += [file_record(HERE/n) for n in ['CALLER-TARGET.json','INPUTS.json','dependency-closure.json','dependency-symlinks.json','controls/metadata/result.json']]
 plan['dependencies']=file_record(HERE/'dependency-inputs.json');plan['input_symlinks']+=json_read(HERE/'dependency-symlinks.json')
 if name!='compile':
  compiled=json_read(HERE/'controls/compile/result.json');require(compiled['accepted'] and compiled['terminal_authenticated'] and compiled['inputs_unchanged'] and compiled['raw_status']==0,'compile not qualified')
  artifacts=json_read(HERE/'retained-binaries/ARTIFACTS.json');require(set(artifacts)=={'ptrace','vsyscall'},'wrong retained set')
  plan['inputs'] += [file_record(HERE/n) for n in ['controls/compile/result.json','controls/compile/artifacts.json','retained-binaries/ARTIFACTS.json','retained-binaries/BINDING.json']]
  for key,item in artifacts.items():
   artifact=item['file'];check_file(artifact,executable=True);require(artifact['path']==str(HERE/'retained-binaries'/(key+'.elf')),'wrong retained path')
   plan['inputs'].append(artifact);extra,aliases=loader(key,artifact,plan['environment']);plan['inputs']+=extra;plan['input_symlinks']+=aliases
  for retained in json_read(HERE/'retained-binaries/BINDING.json')['retained_libraries']:
   check_file(retained['retained']);plan['inputs'].append(retained['retained'])
 if row['kind']=='test':
  inventory_path=HERE/'controls'/('list-'+row['harness'])/'result.json';inventory=json_read(inventory_path)
  require(inventory['accepted'] and inventory['terminal_authenticated'] and inventory['inputs_unchanged'] and inventory['raw_status']==0,'inventory not qualified')
  require(len(row['selected'])==1 and inventory['readback']['names'].count(row['selected'][0])==1,'test absent or ambiguous');plan['inputs'].append(file_record(inventory_path))
 plan['inputs']=unique(plan['inputs']);plan['input_symlinks']=unique(plan['input_symlinks'])
 write_new(Path(plan['path']),plan);print(plan['path'],digest(plan['path']))
if __name__=='__main__':main()
