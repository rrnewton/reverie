"""Local evidence helpers; importing this module launches no command."""

import hashlib
import json
from pathlib import Path

ROOT = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918')
OUT = ROOT / 'ignored/kvm-owner-activation-v2'
TOOLCHAIN = Path('/home/newton/.rustup/toolchains/nightly-2026-07-29-x86_64-unknown-linux-gnu/bin')
TARGET = ROOT / 'target/parity-composition-v1-cold-v1'
STREAM_CAP = 16 * 1024 * 1024


def identity(path):
    path = Path(path)
    digest = hashlib.sha256()
    size = 0
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
            size += len(block)
    return {'bytes': size, 'sha256': digest.hexdigest()}


def write_json(path, value):
    with Path(path).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def local_path(name):
    path = (OUT / name).resolve()
    if path == OUT or not path.is_relative_to(OUT):
        raise ValueError('output/input path must remain under the named evidence directory')
    return path


def load_bound(name, expected):
    path = local_path(name)
    actual = identity(path)
    if actual['sha256'] != expected:
        raise ValueError(f'{name}: expected {expected}, found {actual["sha256"]}')
    return json.loads(path.read_text()), actual


def validate_plan(plan):
    if plan['root'] != str(ROOT) or plan.get('status') != 'ready for root-authorized execution':
        raise ValueError('plan is not finalized for the assigned slot')
    paths = plan['source_paths']
    if len(paths) != len(set(paths)):
        raise ValueError('duplicate source path')
    for name in paths:
        if not (ROOT / name).resolve().is_relative_to(ROOT):
            raise ValueError('source escapes assigned slot')
    all_names = []
    for group in plan['tests']:
        names = group['expected_names']
        if not names or group['expected_passed'] != len(names):
            raise ValueError(f'unresolved or inconsistent selection: {group["phase"]}')
        if not group['selector'] or len(names) != len(set(names)):
            raise ValueError('invalid selector or duplicate selected test')
        all_names.extend(names)
    if len(all_names) != len(set(all_names)):
        raise ValueError('test selected more than once')


def source_identity(plan):
    return {name: identity(ROOT / name) for name in plan['source_paths']}


def load_source(name, expected, plan, plan_sha):
    source, bound = load_bound(name, expected)
    if source['plan_sha256'] != plan_sha or set(source['source']) != set(plan['source_paths']):
        raise ValueError('source snapshot is not bound to this complete plan')
    for item, record in source['source'].items():
        if identity(local_path(source['snapshot_directory']) / item) != record:
            raise ValueError(f'retained source snapshot differs: {item}')
    if source_identity(plan) != source['source']:
        raise ValueError('live source/lock identity differs from the frozen snapshot')
    return source, bound


def selected_names(plan, inventory):
    selections = {}
    for group in plan['tests']:
        selector = group['selector']
        actual = sorted(name for name in inventory if
                        (name == selector if group['exact'] else selector in name))
        if actual != sorted(group['expected_names']):
            raise ValueError(f'inventory selection differs for {group["phase"]}: {actual}')
        selections[group['phase']] = actual
    return selections
