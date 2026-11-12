#!/usr/bin/python3
"""Read back all discovered inputs and retained query results; no execution."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat

from bind_inputs import digest, link_chain, ROOT

HERMIT = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-prejoin-main-20260917')


def file_record(path):
    path = Path(path)
    s = path.stat()
    return {'path': str(path), 'size': s.st_size, 'mode': stat.S_IMODE(s.st_mode), 'sha256': digest(path)}


def main():
    checks = []
    def check(name, condition):
        checks.append({'check': name, 'passed': bool(condition)})
        if not condition:
            raise AssertionError(name)

    inputs = json.loads((ROOT/'inputs-complete.json').read_text())
    for row in inputs:
        path = Path(row['path'])
        resolved = path.resolve(strict=True)
        s = resolved.stat()
        check('path resolution: '+str(path), str(resolved) == row['resolved_path'])
        check('metadata: '+str(path),
              (s.st_size, stat.S_IMODE(s.st_mode), s.st_dev, s.st_ino, s.st_mtime_ns) ==
              (row['size'], row['mode'], row['device'], row['inode'], row['mtime_ns']))
        check('contents: '+str(path), digest(resolved) == row['sha256'])
        check('symlink chain: '+str(path), link_chain(path) == row['symlinks'])

    perl = json.loads((ROOT/'perl-inputs.json').read_text())
    candidate_rows = list(perl['sitecustomize'])
    for item in perl['module_candidates'] + perl['extensions']:
        candidate_rows.extend(item['candidates'])
    lookups = json.loads((ROOT/'lookup-inputs.json').read_text())
    for item in lookups['commands']:
        candidate_rows.extend(item['attempts_through_selected'])
    for item in candidate_rows:
        path = Path(item['path'])
        check('candidate presence: '+str(path), path.exists() == item['exists'] and os.path.lexists(path) == item['lexists'])
        if 'executable' in item:
            check('PATH executability: '+str(path), (os.access(path, os.X_OK) and path.is_file()) == item['executable'])
    check('no loader preload', not os.path.lexists('/etc/ld.so.preload'))

    context_path = HERMIT/'ignored/prejoin-main-callers-v8/compile-input-context.json'
    context = json.loads(context_path.read_text())
    environment = json.loads((ROOT/'environment.json').read_text())
    check('query environment equals supplied context', environment == context['environment'])
    check('protected overrides absent', not any(name in environment for name in lookups['environment_keys_absent']))
    check('PATH lookup environment bound', lookups['environment_sha256'] == digest(ROOT/'environment.json'))
    plan = HERMIT/'ignored/prejoin-main-guest-plan-v1'
    check('prerequisites bytes retained', (plan/'guest-prerequisites.json').read_bytes() == (ROOT/'guest-prerequisites.json').read_bytes())
    source_files = []
    for relative in ['hermit-cli/tests/cli.rs', 'hermit-cli/tests/kvm_harder.rs']:
        source = HERMIT/relative
        frozen = plan/'source'/relative
        check('original test source equals frozen copy: '+relative, source.read_bytes() == frozen.read_bytes())
        source_files.extend([file_record(source), file_record(frozen)])
    cli = (HERMIT/'hermit-cli/tests/cli.rs').read_text()
    names = json.loads((ROOT/'original-24-names.json').read_text())
    check('original CLI selection size', len(names) == 24)
    for name in names:
        check('original method retained: '+name, 'fn '+name+'(' in cli)
    for fixture in json.loads((ROOT/'fixtures.json').read_text()):
        copy = Path(fixture['copy'])
        check('fixture hash: '+fixture['name'], digest(copy) == fixture['sha256'])
        if fixture['kind'] == 'repository_fixture':
            check('original fixture bytes: '+fixture['name'], Path(fixture['source']).read_bytes() == copy.read_bytes())
        else:
            part = cli.split('fn '+fixture['source_method']+'(',1)[1]
            start = part.index('br#"') + 4
            end = part.index('"#', start)
            check('original inline bytes: '+fixture['name'], part[start:end].encode() == copy.read_bytes())
        check('no compiled discovery output: '+fixture['name'], not os.path.lexists(ROOT/('NOT-BUILT-'+fixture['name'])))

    queries = []
    for result_path in sorted((ROOT/'queries').glob('*/result.json')):
        record = json.loads(result_path.read_text())
        check('query success: '+record['name'], record['exit_status'] == 0 and not record['timed_out'])
        check('query environment: '+record['name'], record['environment_sha256'] == digest(ROOT/'environment.json'))
        for stream in record['streams'].values():
            path = Path(stream['path'])
            check('query retained stream: '+str(path), path.stat().st_size == stream['bytes'] and digest(path) == stream['sha256'])
            check('query output bound: '+str(path), stream['bytes'] < record['bounds']['file_bytes_per_stream'])
        queries.append({'result': file_record(result_path), 'name': record['name'], 'argv': record['argv'],
                        'cpu_seconds': record['cpu_seconds'], 'wall_seconds': record['wall_seconds']})
    elf = json.loads((ROOT/'elf-dependencies-complete.json').read_text())
    check('all static ELF queries represented', {x['name'] for x in queries if x['name'].startswith('elf-')} == {x['query'] for x in elf})
    check('required inputs available', not json.loads((ROOT/'unavailable-complete.json').read_text())['required_unavailable'])
    check('all DT_NEEDED resolved', all(item['candidates'] for row in elf for item in row['resolutions']))

    value = {
        'status': 'All read-only input and query checks passed; no guest build or execution',
        'files': len(inputs), 'unique_elf_files': len(elf), 'check_count': len(checks),
        'queries': queries, 'query_count': len(queries),
        'query_cpu_seconds_sum': sum(row['cpu_seconds'] for row in queries),
        'query_wall_seconds_sum': sum(row['wall_seconds'] for row in queries),
        'source_inputs': source_files + [file_record(context_path), file_record(plan/'guest-prerequisites.json')],
        'input_records': [file_record(ROOT/name) for name in ['inputs-complete.json','elf-dependencies-complete.json',
                          'unavailable-complete.json','perl-inputs.json','lookup-inputs.json','environment.json',
                          'fixtures.json','original-24-names.json','original-method-excerpts.json']],
        'checks': checks,
    }
    with (ROOT/'READBACK.json').open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
    print(json.dumps({key: value[key] for key in ['status','files','unique_elf_files','check_count','query_count',
                                                'query_cpu_seconds_sum','query_wall_seconds_sum']}))


if __name__ == '__main__':
    main()
