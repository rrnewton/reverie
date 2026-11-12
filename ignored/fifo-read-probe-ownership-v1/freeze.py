import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-reverie-20260918')
D = ROOT / 'ignored/fifo-read-probe-ownership-v1'
REV = '000c15a1161ea2d58749431b5ddaaa97f7aa37d5'
H = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-replay-prerequisites-20260918/ignored/timer-integration-review-v27')
OLD = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-setitimer-20260918/ignored/kvm-named-fifo-analysis-20260918')
ENV = dict(os.environ, GIT_NO_LAZY_FETCH='1', GIT_OPTIONAL_LOCKS='0')


def digest(data):
    return hashlib.sha256(data).hexdigest()


def record(path):
    data = path.read_bytes()
    return {'path': str(path), 'bytes': len(data), 'sha256': digest(data)}


def git(*args):
    return subprocess.run(['git', '-c', 'core.fsmonitor=false', *args], cwd=ROOT,
                          env=ENV, check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, timeout=30).stdout


def retain(destination, data):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open('xb') as output:
        output.write(data)
    assert destination.read_bytes() == data
    return record(destination)


assert not (D / 'INPUTS.json').exists()
assert not (D / 'READBACK.json').exists()
head = git('rev-parse', 'HEAD').decode().strip()
tree = git('rev-parse', 'HEAD^{tree}').decode().strip()
assert head == REV
assert tree == 'a12d56466eed51cdd7d12087b5fafa9d66d8c864'
assert git('status', '--porcelain=v1', '--untracked-files=no') == b''
sources = []
for rel in ['reverie-kvm/src/executor.rs', 'reverie-kvm/src/elf.rs',
            'reverie-kvm/src/vm.rs', 'reverie-kvm/src/runtime.rs']:
    data = git('show', REV + ':' + rel)
    assert (ROOT / rel).read_bytes() == data
    blob = git('rev-parse', REV + ':' + rel).decode().strip()
    assert hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest() == blob
    sources.append({'repository': 'reverie', 'revision': REV, 'relative_path': rel,
                    'git_blob': blob, 'live': record(ROOT / rel),
                    'retained': retain(D / 'source/reverie' / rel, data)})

hmanifest = json.loads((H / 'SOURCE-COPY.json').read_text())
assert hmanifest['head'] == 'e740bdcf42153fc986d3b58891ad7a7ba9546dce'
entries = {entry['path']: entry for entry in hmanifest['entries']}
for rel in ['detcore/src/fd.rs', 'detcore/src/syscalls/files.rs',
            'detcore/src/syscalls/helpers.rs', 'detcore/src/tool_local.rs']:
    original = H / 'source' / rel
    bound = entries[rel]['snapshot']
    data = original.read_bytes()
    assert digest(data) == bound['sha256'] and len(data) == bound['bytes']
    sources.append({'repository': 'hermit', 'revision': hmanifest['head'],
                    'relative_path': rel, 'frozen_original': record(original),
                    'retained': retain(D / 'source/hermit' / rel, data)})

references = []
for parent, label, names in [
    (H, 'hermit-v27-bindings', ['PACKET.json', 'SOURCE-COPY.json']),
    (OLD, 'historical-fifo', ['REPORT.md', 'INPUTS.json', 'READBACK.json', 'SOURCE_BINDINGS.json']),
]:
    for name in names:
        path = parent / name
        references.append({'original': record(path),
                           'retained': retain(D / 'references' / label / name, path.read_bytes())})

fetch = json.loads((D / 'FETCH.json').read_text())
requests = fetch['requests'] + [json.loads((D / 'FETCH-OPEN.json').read_text())]
assert len(requests) == 11
primary = []
for request in requests:
    assert request['rc'] == 0
    def absolute(value):
        path = Path(value)
        return path if path.is_absolute() else ROOT / path
    raw = absolute(request['raw'])
    decoded = absolute(request['decoded'])
    error = absolute(request['stderr'])
    assert digest(raw.read_bytes()) == request['raw_sha256']
    assert len(raw.read_bytes()) == request['raw_bytes']
    assert digest(decoded.read_bytes()) == request['sha256']
    assert len(decoded.read_bytes()) == request['bytes']
    assert error.read_bytes() == b''
    entry = {'url': request['url'], 'raw': record(raw), 'decoded': record(decoded),
             'stderr': record(error), 'fetch_rc': request['rc']}
    if 'git_blob' in request:
        api = json.loads(raw.read_bytes())
        data = base64.b64decode(api['content'])
        assert data == decoded.read_bytes() and len(data) == api['size']
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        assert blob == api['sha'] == request['git_blob']
        entry.update(revision=request['revision'], git_blob=blob)
    primary.append(entry)

inputs = {
    'scope': 'Authored source-only owned FIFO/read-probe plan; no implementation, guest execution, native probe, build, or independent approval.',
    'reverie': {'head': head, 'tree': tree, 'tracked_clean': True},
    'source_read_coverage': 'Focused complete functions and surrounding production paths named in REPORT; retained whole files are not a claim of complete scheduler grounding.',
    'sources': sources, 'prior_references': references, 'primary_references': primary,
    'primary_limit': 'Upstream v7.1.3 and retained man-pages sources, not authenticated host-vendor source or measured kernel behavior.',
    'report': record(D / 'REPORT.md'),
    'fetch_logs': [record(D / 'FETCH.json'), record(D / 'FETCH-OPEN.json')],
}
retain(D / 'INPUTS.json', (json.dumps(inputs, indent=2) + '\n').encode())
assert git('rev-parse', 'HEAD').decode().strip() == head
assert git('status', '--porcelain=v1', '--untracked-files=no') == b''
for item in sources:
    original = item.get('live', item.get('frozen_original'))
    assert record(Path(original['path'])) == original
inventory = [record(path) for path in sorted(D.rglob('*')) if path.is_file()]
readback = {
    'scope': 'Append-only frozen research artifacts; all listed bytes read back. No original source, old report, index, ref or cache was changed.',
    'head_before_after': head, 'tree_before_after': tree, 'tracked_clean_before_after': True,
    'files': inventory, 'file_count': len(inventory), 'executions': [],
    'product_writes': [], 'new_native_or_guest_results': [],
}
retain(D / 'READBACK.json', (json.dumps(readback, indent=2) + '\n').encode())
for name in ['REPORT.md', 'INPUTS.json', 'READBACK.json']:
    print(json.dumps(record(D / name)))
