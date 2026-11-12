#!/usr/bin/env python3
"""Read metadata from existing cache ELFs; never execute the cache artifacts."""
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import signal
import subprocess
import time

ROOT = Path(__file__).resolve().parent


def limits():
    resource.setrlimit(resource.RLIMIT_CPU, (5, 5))
    resource.setrlimit(resource.RLIMIT_FSIZE, (1024**2, 1024**2))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def main():
    existing = json.loads((ROOT/'OBSERVED-FILES.json').read_text())
    prior = json.loads((ROOT.parent/'inputs-complete.json').read_text())
    prior_map = {row['path']: row for row in prior}
    readelf = Path('/usr/bin/readelf')
    assert hashlib.sha256(readelf.read_bytes()).hexdigest() == prior_map[str(readelf)]['sha256']
    metadata_prior = {row['path']: row for row in json.loads((ROOT.parent/'elf-dependencies-complete.json').read_text())}
    name_candidates = {}
    for row in metadata_prior.values():
        for dependency in row['resolutions']:
            name_candidates[dependency['name']] = dependency['candidates']
    todo = [row['file'] for row in json.loads((ROOT/'GENERATED-COMMANDS.json').read_text())]
    todo += json.loads((ROOT/'GENERATED-PROC-MACROS.json').read_text())
    queries = ROOT/'elf-queries'
    queries.mkdir(exist_ok=False)
    result = []
    dependencies = {}
    for index, bound in enumerate(todo):
        path = Path(bound['path'])
        assert hashlib.sha256(path.read_bytes()).hexdigest() == bound['sha256']
        directory = queries/f'{index:02}'
        directory.mkdir()
        argv = ['/usr/bin/readelf', '-W', '-l', '-d', str(path)]
        started = time.monotonic()
        with (directory/'stdout').open('xb') as out, (directory/'stderr').open('xb') as err:
            process = subprocess.Popen(argv, stdout=out, stderr=err, start_new_session=True,
                                       preexec_fn=limits, stdin=subprocess.DEVNULL)
            try:
                status = process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise
        record = {'argv': argv, 'pid': process.pid, 'exit_code': status, 'wall_seconds': time.monotonic()-started,
                  'bounds': {'cpu_seconds': 5, 'wall_seconds': 15, 'bytes_per_stream': 1024**2}}
        for channel in ['stdout', 'stderr']:
            data = (directory/channel).read_bytes()
            record[channel] = {'path': str(directory/channel), 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
        with (directory/'result.json').open('x') as stream:
            json.dump(record, stream, indent=2)
            stream.write('\n')
        assert status == 0
        assert hashlib.sha256(path.read_bytes()).hexdigest() == bound['sha256']
        text = (directory/'stdout').read_text()
        needed = re.findall(r'\(NEEDED\).*Shared library: \[(.*?)\]', text)
        interpreters = re.findall(r'Requesting program interpreter: (.*?)\]', text)
        runpath = re.findall(r'\((?:RUNPATH|RPATH)\).*Library (?:runpath|rpath): \[(.*?)\]', text)
        assert not runpath
        resolved = []
        for name in needed:
            assert name in name_candidates, name
            candidates = name_candidates[name]
            for candidate in candidates:
                deps = [candidate['path']]
                while deps:
                    dep = deps.pop()
                    if dep in dependencies:
                        continue
                    old = prior_map[dep]
                    assert hashlib.sha256(Path(dep).read_bytes()).hexdigest() == old['sha256']
                    dependencies[dep] = old
                    previous = metadata_prior[old['resolved_path']]
                    deps += previous['interpreters']
                    deps += [c['path'] for d in previous['resolutions'] for c in d['candidates']]
            resolved.append({'name': name, 'candidates': candidates})
        for interp in interpreters:
            old = prior_map[interp]
            assert hashlib.sha256(Path(interp).read_bytes()).hexdigest() == old['sha256']
            dependencies[interp] = old
        result.append({'artifact': bound, 'interpreter': interpreters, 'needed': resolved,
                       'query': str(directory/'result.json'), 'scope': 'Static ELF metadata only'})
    with (ROOT/'GENERATED-ELF-DEPENDENCIES.json').open('x') as stream:
        json.dump(result, stream, indent=2)
        stream.write('\n')
    with (ROOT/'GENERATED-LOADER-INPUTS.json').open('x') as stream:
        json.dump(sorted(dependencies.values(), key=lambda row: row['path']), stream, indent=2)
        stream.write('\n')
    print(json.dumps({'elf_objects': len(result), 'loader_inputs': len(dependencies)}))


if __name__ == '__main__':
    main()
