#!/usr/bin/python3
"""Record two independent, source-bound workspace lint checks."""
from pathlib import Path
import hashlib
import json
import os
import runpy
import subprocess

HERE = Path(__file__).resolve().parent
PLAN_SHA256 = 'f5fb145b36dbb665218328af20a80d1c8f8d2408a67122def5443bd56a9a4306'


def main():
    raw = (HERE / 'plan.json').read_bytes()
    assert hashlib.sha256(raw).hexdigest() == PLAN_SHA256
    plan = json.loads(raw)
    helper = Path(plan['helpers']['path'])
    assert hashlib.sha256(helper.read_bytes()).hexdigest() == plan['helpers']['sha256']
    functions = runpy.run_path(str(helper), run_name='reviewed_helpers_only')
    require, digest, read_bounded = (functions[name] for name in ['require', 'digest', 'read_bounded'])
    write_new = functions['write_new']
    require(plan['execution'] == ['/usr/bin/python3', '-B', str(HERE / 'launch.py')], 'wrong caller')
    require([step['name'] for step in plan['stages']] == ['format', 'clippy'], 'unexpected check')
    for step in plan['stages']:
        require(step['payload'] == step['argv'][step['argv'].index('--log-bytes') + 2:],
                'payload differs from actual argv: ' + step['name'])
    root = Path(plan['run_root'])
    target = Path(plan['target_dir'])
    require(plan['reuse_owned_target_cache'] is True and target.is_dir() and not target.is_symlink(),
            'missing owned Cargo cache')
    require(target.stat().st_uid == os.getuid() and target.resolve(strict=True).is_relative_to(
            Path(plan['source_root']) / 'target'), 'Cargo cache is outside owned target')
    for path in [root, Path(plan['observer_root'])]:
        require(not path.exists() and not path.is_symlink(), 'retain previous attempt: ' + str(path))
    functions['check_inputs'](plan)
    root.mkdir(mode=0o700)
    Path(plan['tmpdir']).mkdir(mode=0o700)
    environment = {key: os.environ[key] for key in plan['environment_keys'] if key in os.environ}
    environment.update(plan['environment_fixed'])
    write_new(root / 'launch.json', dict(plan_sha256=PLAN_SHA256, caller_sha256=digest(__file__),
              execution=plan['execution'], cwd=str(Path.cwd()), environment=environment,
              source_binding=plan['source_binding'], scope=plan['scope']))
    outcomes = []
    for step in plan['stages']:
        # Formatting and Clippy are independent read-only source checks. A
        # nonzero check is retained and does not suppress the other check.
        # Changed source/input bytes, however, prevent any subsequent launch.
        functions['check_inputs'](plan)
        outcome = dict(stage=step['name'])
        try:
            write_new(root / (step['name'] + '-dispatch.json'), dict(argv=step['argv'], cwd=step['cwd']))
            with (root / (step['name'] + '-observer.stdout')).open('xb') as stdout, \
                 (root / (step['name'] + '-observer.stderr')).open('xb') as stderr:
                process = subprocess.run(step['argv'], cwd=step['cwd'], env=environment,
                                         stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr)
            result_path = Path(step['out']) / 'result.json'
            result = json.loads(read_bounded(result_path, 1024**2))
            write_new(root / (step['name'] + '-readback.json'), dict(observer_exit=process.returncode,
                      result_path=str(result_path), result_sha256=digest(result_path), result=result))
            functions['require_terminal'](result, process.returncode, step, root, environment)
            read_bounded(Path(step['out']) / 'stdout', step['reader_limit_bytes'])
            read_bounded(Path(step['out']) / 'stderr', step['reader_limit_bytes'])
            outcome['status'] = 'passed'
        except Exception as error:
            outcome.update(status='failed', error=str(error))
            # The shared helper reaches this exact error only after all
            # service/accounting/bound checks and the independent empty
            # readback passed. An observation or cleanup refusal stops here.
            outcome['stop_remaining_checks'] = str(error) != step['name'] + ' failed'
        outcomes.append(outcome)
        write_new(root / (step['name'] + '-outcome.json'), outcome)
        functions['check_inputs'](plan)
        if outcome.get('stop_remaining_checks', False):
            break
    status = 'passed' if all(row['status'] == 'passed' for row in outcomes) else 'failed'
    write_new(root / 'summary.json', dict(status=status, outcomes=outcomes, scope=plan['scope']))
    print(json.dumps(dict(status=status, summary=str(root / 'summary.json'))))
    if status != 'passed':
        raise SystemExit(1)


if __name__ == '__main__':
    main()
