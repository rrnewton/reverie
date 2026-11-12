#!/usr/bin/python3
"""Bind observed header membership or retain an already qualified native ELF."""
import os
from pathlib import Path
import shutil
import sys
import cache_lease
from common import check_file, file_record, json_read, require, write_new
from native_phase import check

HERE=Path(__file__).resolve().parent

def main():
    require(sys.argv[1:] in [['headers'],['elf']],'expected headers|elf')
    stage='dependencies' if sys.argv[1]=='headers' else 'compile'
    plan_path=HERE/'plans'/(stage+'.json');plan_record=file_record(plan_path);plan=json_read(plan_path)
    result_path=HERE/'controls'/stage/'result.json';result=json_read(result_path)
    require(result['accepted'] and result['terminal_authenticated'] and result['inputs_unchanged'],'phase did not qualify')
    require(result['plan_sha256']==plan_record['sha256'],'wrong phase plan')
    fd=cache_lease.join(plan['lease'],plan_record['sha256'])
    try:
        check(plan)
        if stage=='dependencies':
            files,links={},{}
            names=result['readback']['dependencies']
            for name in names:
                p=Path(name)
                for part in [p,*p.parents]:
                    if part.is_symlink():links[str(part)]=dict(path=str(part),target=os.readlink(part),mode=part.lstat().st_mode&0o7777)
                resolved=p.resolve(strict=True);files[str(resolved)]=file_record(resolved)
            for r in files.values():check_file(r)
            write_new(HERE/'HEADER-INPUTS.json',dict(compiler_discovery_result=file_record(result_path),
                discovery_stdout=result['stdout'],source=json_read(HERE/'SETUP.json')['source'],spelling_names=names,
                files=list(files.values()),input_symlinks=list(links.values()),
                scope='Compiler dependency discovery only; these exact bytes/modes/aliases are bound before actual compile and actual compile membership must match.'))
        else:
            original=result['readback']['binary'];check_file(original,executable=True)
            source=Path(original['path']);require(source==HERE/'target/probe' and not source.is_symlink(),'wrong compiled path')
            destination=HERE/'retained-binaries/probe'
            with source.open('rb') as inp, destination.open('xb') as out:
                shutil.copyfileobj(inp,out,1024**2);out.flush();os.fsync(out.fileno())
            destination.chmod(0o555)
            retained=file_record(destination);check_file(original,executable=True)
            a=source.stat();b=destination.stat()
            require(retained['sha256']==original['sha256'] and retained['bytes']==original['bytes']
                    and (a.st_dev,a.st_ino)!=(b.st_dev,b.st_ino),'retained ELF is not an identical independent copy')
            write_new(HERE/'retained-binaries/BINDING.json',dict(compile_result=file_record(result_path),compile_plan=plan_record,
                original=original,retained=retained,original_identity=[a.st_dev,a.st_ino,a.st_uid],retained_identity=[b.st_dev,b.st_ino,b.st_uid],
                source=json_read(HERE/'SETUP.json')['source'],header_inputs=file_record(HERE/'HEADER-INPUTS.json'),
                diagnostics=0,distinct_inode=True,not_a_rust_or_hermit_artifact=True))
        check(plan);cache_lease.check_held(fd,plan['lease'],plan_record['sha256'])
    finally:
        os.close(fd)

if __name__=='__main__':
    main()
