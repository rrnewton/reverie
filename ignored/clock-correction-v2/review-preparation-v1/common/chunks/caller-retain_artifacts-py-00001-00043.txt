#!/usr/bin/python3
"""Retain both fresh ptrace test ELFs and local libraries before later Cargo phases."""
import json,os
from pathlib import Path
from common import check_file,digest,file_record,json_read,read,require,write_new
from phase import check
HERE=Path(__file__).resolve().parent
REPO=Path(json_read(HERE/'SETUP.json')['source_root'])
plan=json_read(HERE/'compile-plan.json');result=json_read(HERE/'controls/compile/result.json')
require(result['accepted'] and result['terminal_authenticated'] and result['inputs_unchanged'] and result['raw_status']==0,'compile did not qualify')
require(result['plan_sha256']==digest(HERE/'compile-plan.json'),'compile plan differs');check(plan)
originals=json_read(HERE/'controls/compile/artifacts.json');require(set(originals)=={'ptrace','vsyscall'},'wrong artifact selection')
retained=HERE/'retained-binaries';retained.mkdir(mode=0o700)
def copy(original,destination):
 check_file(original);source=Path(original['path']);require(source.resolve()==source and source.is_relative_to(Path(plan['target']['path'])),'artifact outside target')
 with source.open('rb') as src,destination.open('xb') as dst:
  while block:=src.read(1024**2):dst.write(block)
  dst.flush();os.fsync(dst.fileno())
 destination.chmod(0o555 if destination.suffix in ('.elf','.so') else 0o444)
 actual=file_record(destination);require(actual['sha256']==original['sha256'] and actual['bytes']==original['bytes'],'retained bytes differ')
 require((source.stat().st_dev,source.stat().st_ino)!=(destination.stat().st_dev,destination.stat().st_ino),'retained inode aliases target');check_file(original)
 return dict(original=original,retained=actual,distinct_inode=True)
harnesses={};artifacts={}
for selector in plan['artifact_selectors']:
 key=selector['id'];row=originals[key];cargo=row['cargo_artifact']
 require(cargo.get('fresh') is False and cargo['manifest_path']==str(REPO/'reverie-ptrace/Cargo.toml') and cargo['target']['name']==selector['target'] and cargo['target']['kind']==selector['kind'] and cargo['profile']['test'] is True,'fresh selected test harness required')
 check_file(row['file'],executable=True)
 with Path(row['file']['path']).open('rb') as f:require(f.read(4)==b'\x7fELF','selected harness not ELF')
 harnesses[key]=copy(row['file'],retained/(key+'.elf'))
 artifacts[key]=dict(cargo_artifact=cargo,file=harnesses[key]['retained'])
prior=json_read(HERE/'HISTORY.json')['previous_failed_elf'];check_file(prior,executable=True)
require(artifacts['ptrace']['file']['sha256']!=prior['sha256'],'new library harness reused prior failed bytes')
libraries=[];seen=set();raw_path=Path(plan['output'])/'stdout'
for item in [json.loads(x) for x in read(raw_path).splitlines()]:
 if item.get('reason')!='compiler-artifact' or not Path(item.get('manifest_path','/')).is_relative_to(REPO):continue
 for name in item['filenames']:
  path=Path(name)
  if path.suffix not in ('.rlib','.so') or name in seen:continue
  seen.add(name);libraries.append(dict(cargo_artifact=item,**copy(file_record(path),retained/('library-'+str(len(libraries))+'-'+path.name))))
require(libraries,'no locally emitted library artifact retained');check(plan)
write_new(retained/'BINDING.json',dict(compile_plan=file_record(HERE/'compile-plan.json'),compile_result=file_record(HERE/'controls/compile/result.json'),cargo_output=file_record(raw_path),source_manifests=plan['sources'],original_artifacts=file_record(HERE/'controls/compile/artifacts.json'),harnesses=harnesses,retained_libraries=libraries,all_selected_harnesses_newly_built=True,previous_failed_harness=prior,meaning='Two freshly emitted retained test ELFs and local libraries; no transferred test results.'))
write_new(retained/'ARTIFACTS.json',artifacts)
print(json.dumps(dict(harnesses={k:v['file'] for k,v in artifacts.items()},retained_local_libraries=len(libraries))))
