#!/usr/bin/python3
"""Run the approved finite sequence once; never recover, retry, or change a plan."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from common import check_file, digest, file_record, json_read, require, write_new

HERE = Path(__file__).resolve().parent
PYTHON = '/usr/bin/python3'

def authenticate():
    target = json_read(HERE/'CALLER-TARGET.json')
    check_file(target['inputs'])
    index = json_read(target['inputs']['path'])
    for record in index['files']:
        check_file(record)
    for record in index['symlinks']:
        path = Path(record['path'])
        require(path.is_symlink() and os.readlink(path) == record['target'], 'held alias differs: '+str(path))
    for record in index['unexpanded_gitlinks']:
        path=Path(record['path'])
        require(path.is_dir() and not path.is_symlink(), 'gitlink carrier differs: '+str(path))
    for path in index['must_be_absent_before_execution']:
        require(not Path(path).exists() and not Path(path).is_symlink(), 'execution destination already exists: '+path)


def command(label, argv):
    prefix = HERE/'launch'/label
    started = time.monotonic()
    with prefix.with_suffix('.stdout').open('xb') as out, prefix.with_suffix('.stderr').open('xb') as err:
        process = subprocess.run(argv, cwd=HERE, env={**os.environ, 'PYTHONOPTIMIZE':'0', 'PYTHONDONTWRITEBYTECODE':'1'},
                                 stdin=subprocess.DEVNULL, stdout=out, stderr=err, check=False)
    record = dict(argv=argv, returncode=process.returncode, elapsed_seconds=time.monotonic()-started,
                  stdout=file_record(prefix.with_suffix('.stdout')), stderr=file_record(prefix.with_suffix('.stderr')))
    write_new(prefix.with_suffix('.json'), record)
    require(process.returncode == 0, 'step refused/failed; no retry: '+label)
    return record


def main():
    require(sys.flags.optimize == 0 and os.environ.get('PYTHONOPTIMIZE') == '0', 'explicit unoptimized Python required')
    require(len(sys.argv) == 2 and sys.argv[1] == 'run', 'use execute.py run only after owner authorization')
    authenticate()
    selection = json_read(HERE/'SELECTORS.json')
    names = selection['proposed_order']
    require(len(names) == len(set(names)) == 63 and set(names) == set(selection['groups']), 'wrong finite selection')
    exact = [(selection['groups'][n]['artifact'], selection['groups'][n]['names']) for n in names]
    require(all(len(n) == 1 for _, n in exact) and len({(a,n[0]) for a,n in exact}) == 63, 'not 63 unique exact declarations')
    # Admission has no product process; all later product processes use phase.py's unchanged observer.
    subprocess.run([PYTHON, '-B', str(HERE/'admit_target.py')], cwd=HERE, check=True)
    sequence = ['metadata', 'compile', 'format', 'list-lib', 'list-static', 'list-vmcall', 'list-read-clock', *names, 'core-check', 'clippy']
    journal = dict(sequence=sequence, planned_phases=72, completed=[], error=None)
    try:
        for name in sequence:
            command('prepare-'+name, [PYTHON,'-B',str(HERE/'prepare.py'),name])
            plan = HERE/(name+'-plan.json')
            command('phase-'+name, [PYTHON,'-B',str(HERE/'phase.py'),'launch',str(plan),digest(plan)])
            result = json_read(HERE/'controls'/name/'result.json')
            require(result['accepted'] and result['terminal_authenticated'] and result['inputs_unchanged'] and result['raw_status']==0, 'phase did not qualify: '+name)
            journal['completed'].append(name)
            if name == 'metadata':
                command('bind-dependencies', [PYTHON,'-B',str(HERE/'bind_dependencies.py')])
            if name == 'compile':
                command('retain-artifacts', [PYTHON,'-B',str(HERE/'retain_artifacts.py')])
    except BaseException as error:
        journal['error'] = repr(error)
        raise
    finally:
        write_new(HERE/'EXECUTION-RESULT.json', journal)
    require(len(journal['completed']) == 72, 'incomplete finite sequence')
    print(json.dumps(dict(completed_phases=72, exact_tests=63, meaning='phase receipts remain the authority')))

if __name__ == '__main__':
    main()
