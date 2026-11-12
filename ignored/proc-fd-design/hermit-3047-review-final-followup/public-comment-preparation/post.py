from pathlib import Path
import datetime
import hashlib
import json
import subprocess
import sys
import time

p = Path(__file__).resolve().parent
if sys.argv[1:] != ['--post-reviewed-a88f9480']:
    raise SystemExit('Posting disabled: root must read and release this exact caller and body.')
expected_head = 'a88f948021a24c6a1ea808f868516c0691c8819a'
expected_url = 'https://github.com/rrnewton/hermit/pull/3047'
expected_marker = 'APPROVED-AT: claude ' + expected_head
binding = json.loads((p / 'binding.json').read_text())
for row in binding['files']:
    path = Path(row['path'])
    data = path.read_bytes()
    if (len(data) != row['bytes'] or hashlib.sha256(data).hexdigest() != row['sha256']
            or oct(path.stat().st_mode & 0o777) != row['mode']):
        raise RuntimeError('Publication input changed: ' + str(path))
body = (p / 'comment.md').read_bytes()
request = json.loads((p / 'request.json').read_text())
if request != {'body': body.decode()} or body.decode().count(expected_marker) != 1:
    raise RuntimeError('Structured request does not carry the exact reviewed body/marker')
if not body.endswith((p.parent / 'REVIEW.md').read_bytes()):
    raise RuntimeError('Literal final review was modified')
out = p / 'post-1'
out.mkdir(exist_ok=False)


def call(name, args):
    command = ['with-proxy', 'gh', 'api', *args]
    record = {'argv': command, 'started_at': datetime.datetime.now(datetime.timezone.utc).isoformat()}
    (out / (name + '.request.json')).write_text(json.dumps(record, indent=2) + '\n')
    start = time.monotonic()
    try:
        with (out / (name + '.stdout')).open('xb') as stdout, (out / (name + '.stderr')).open('xb') as stderr:
            proc = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=60, check=False)
        record['exit_code'] = proc.returncode
    except subprocess.TimeoutExpired:
        record.update(timed_out=True, elapsed_seconds=time.monotonic() - start)
        (out / (name + '.result.json')).write_text(json.dumps(record, indent=2) + '\n')
        raise RuntimeError('GitHub command timed out; retain possible publication and do not retry automatically: ' + name)
    record.update(elapsed_seconds=time.monotonic() - start,
                  finished_at=datetime.datetime.now(datetime.timezone.utc).isoformat())
    (out / (name + '.result.json')).write_text(json.dumps(record, indent=2) + '\n')
    if proc.returncode != 0:
        raise RuntimeError('GitHub command failed; no retry: ' + name)
    return json.loads((out / (name + '.stdout')).read_text())


def require_head(pr):
    if (pr.get('html_url') != expected_url or pr.get('state') != 'open'
            or pr.get('head', {}).get('sha') != expected_head or pr.get('base', {}).get('ref') != 'main'):
        raise RuntimeError('Public pull request no longer matches the reviewed open head/base')


before = call('before', ['repos/rrnewton/hermit/pulls/3047'])
require_head(before)
posted = call('post', ['--method', 'POST', 'repos/rrnewton/hermit/issues/3047/comments',
                       '--input', str(p / 'request.json')])
comment_id = posted.get('id')
if type(comment_id) is not int or comment_id <= 0:
    raise RuntimeError('POST returned no valid comment ID; inspect retained result before any further action')
actual = call('comment-readback', ['repos/rrnewton/hermit/issues/comments/' + str(comment_id)])
if (not isinstance(actual.get('body'), str) or actual['body'].encode() != body
        or expected_marker not in actual['body']
        or not actual.get('html_url', '').startswith(expected_url + '#issuecomment-')):
    raise RuntimeError('Published comment bytes, marker or URL differ from the reviewed body')
after = call('after', ['repos/rrnewton/hermit/pulls/3047'])
require_head(after)
result = {'pull_request': expected_url, 'head_before': before['head']['sha'],
          'head_after': after['head']['sha'], 'comment_url': actual['html_url'],
          'comment_id': comment_id, 'complete_body_bytes_equal': True,
          'body_bytes': len(body), 'body_sha256': hashlib.sha256(body).hexdigest(),
          'canonical_marker': expected_marker, 'literal_review_suffix_unchanged': True,
          'posted_exactly_once': True, 'no_ready_merge_or_source_or_ref_operation': True,
          'finished_at': datetime.datetime.now(datetime.timezone.utc).isoformat()}
(out / 'READBACK.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result), flush=True)
