import collections
import hashlib
import json
from pathlib import Path
import subprocess
import yaml

HERE = Path(__file__).resolve().parent
ROOT = Path('/home/newton/work/dev-hermit/worktrees/slots/liteinst-owner')
SHA = '4d8f866102882b6eeabdf99cc7e81433cc3c95c5'
ANCHOR = '07c10eb80ac4fafbbe65134b504d77ef7e388d25'
SEED = 'liteinst-baseline-20260904-v1'


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def git(*args):
    return subprocess.check_output(['git', '-C', str(ROOT), *args])


def blob(path):
    return git('show', f'{SHA}:{path}')


def identity(cell, backend=None):
    return '/'.join(cell[key] if key != 'backend' else backend or cell[key]
                    for key in ('lane', 'category', 'test', 'mode', 'backend'))


def write_json(name, value):
    (HERE / name).write_text(json.dumps(value, separators=(',', ':'), sort_keys=True) + '\n')


def comparison_records(value):
    if isinstance(value, dict):
        if 'hermit_sha' in value and 'result' in value:
            yield value
        else:
            for nested in value.values():
                yield from comparison_records(nested)
    elif isinstance(value, list):
        for nested in value:
            yield from comparison_records(nested)


matrix_raw = blob('ci/compat-envelope/cells.json')
matrix = json.loads(matrix_raw)
assert digest(matrix_raw) == 'a35b1ded0c09500e7a8b4e3ad0661dbf64ae95e17e7b8d364970d4827b85e5e2'
indexed = {identity(cell): (index, cell) for index, cell in enumerate(matrix['cells'])}
assert len(indexed) == len(matrix['cells'])
metadata = {}
manifest_sources = {}
for path in git('ls-tree', '-r', '--name-only', SHA, 'tests/e2e/manifests').decode().splitlines():
    if not path.endswith('.yaml'):
        continue
    raw = blob(path)
    manifest_sources[path] = {'sha256': digest(raw), 'git_blob': git('rev-parse', f'{SHA}:{path}').decode().strip()}
    for test in yaml.safe_load(raw).get('test', []):
        assert test['id'] not in metadata
        metadata[test['id']] = (path, test)

rows = []
for key, (index, cell) in sorted(indexed.items()):
    if (cell['backend'], cell['mode'], cell['status']) != ('ptrace', 'verify', 'green'):
        continue
    lite_index, lite = indexed[identity(cell, 'liteinst')]
    manifest, test = metadata[cell['test']]
    build = test.get('build') or {}
    flags = [flag for values in build.values() if isinstance(values, list) for flag in values]
    static = '-static' in flags or any('+crt-static' in flag for flag in flags)
    comparisons = [comparison for observation in cell.get('observations', [])
                   for comparison in comparison_records(observation.get('canonical_comparisons'))]
    anchored = [comparison for comparison in comparisons
                if comparison.get('hermit_sha') == ANCHOR and comparison.get('result') == 'pass'
                and comparison.get('left_info_messages') and comparison.get('right_info_messages')
                and all(count > 0 for count in comparison['left_info_messages'] + comparison['right_info_messages'])]
    rows.append({'identity': key, 'test': cell['test'], 'lane': cell['lane'], 'category': cell['category'],
                 'mode': 'verify', 'ptrace_status': 'green', 'liteinst_status': lite['status'],
                 'ptrace_measurement': cell.get('measurement'), 'liteinst_measurement': lite.get('measurement'),
                 'ptrace_source_index': index, 'liteinst_source_index': lite_index,
                 'manifest': manifest, 'program': test.get('program'), 'build': build,
                 'linkage': 'static' if static else 'unknown',
                 'linkage_basis': 'explicit static build flag' if static else 'no explicit linkage proof; default compiler behavior not inferred',
                 'cached_evidence_subset': bool(anchored),
                 'cached_comparison': anchored[0] if anchored else None,
                 'liteinst_disabled_reason': lite.get('ci_disabled_reason') or lite.get('not_applicable_reason')})

assert len(rows) == 340
assert sum(row['cached_evidence_subset'] for row in rows) == 253
status_counts = dict(collections.Counter(row['liteinst_status'] for row in rows))
assert status_counts == {'green': 28, 'red': 23, 'not-applicable': 289}
population = ''.join(row['identity'] + '\n' for row in rows).encode()
(HERE / 'population-identities.txt').write_bytes(population)
columns = ['identity', 'ptrace_status', 'liteinst_status', 'linkage', 'cached_evidence_subset',
           'ptrace_source_index', 'liteinst_source_index', 'manifest', 'program']
(HERE / 'baseline.tsv').write_text('\t'.join(columns) + '\n' + ''.join(
    '\t'.join(str(row[column]) if row[column] is not None else '' for column in columns) + '\n' for row in rows))
linkage_counts = {kind: sum(row['linkage'] == kind for row in rows) for kind in ('static', 'dynamic', 'unknown')}
assert linkage_counts == {'static': 4, 'dynamic': 0, 'unknown': 336}
reference = {row['identity']: row for row in rows}
sample_input = json.loads((HERE / 'sampling-input.json').read_text())
candidates = []
for original in sample_input['ranked_candidates']:
    key = original['identity']
    rank = digest((SEED + '\0' + key).encode())
    assert rank == original['rank_sha256']
    paired = reference.get(key.rsplit('/', 1)[0] + '/ptrace')
    duration = original.get('ptrace_duration') or {}
    intended_dynamic = bool(paired and str(paired['program']).endswith('.c') and paired['linkage'] != 'static')
    eligible = bool(paired and paired['cached_evidence_subset'] and intended_dynamic
                    and duration.get('max_wall_ms') is not None and duration['max_wall_ms'] <= 3000)
    assert eligible == original['eligible']
    candidates.append({'identity': key, 'rank_sha256': rank, 'eligible': eligible,
                       'paired_in_full_reference': paired is not None,
                       'linkage': paired['linkage'] if paired else 'unknown',
                       'manifest_intended_dynamic_sampling_heuristic': intended_dynamic,
                       'historical_ptrace_duration': duration})
candidates.sort(key=lambda candidate: (candidate['rank_sha256'], candidate['identity']))
sample = [candidate for candidate in candidates if candidate['eligible']][:3]
assert [candidate['identity'] for candidate in sample] == sample_input['selected_identities']
summary = {'schema': 1, 'frozen_hermit_sha': SHA, 'reverie_pin': '320412c5967790939ebe405c73e394ffd9c41459',
           'definition': 'ALL backend=ptrace, mode=verify, status=green labels; no linkage or evidence exclusions',
           'reference_count': 340, 'population_sha256': digest(population),
           'historical_liteinst_labels': status_counts, 'cached_evidence_subset_count': 253,
           'missing_cached_evidence_count': 87, 'cached_evidence_anchor': ANCHOR,
           'cached_evidence_is_independent_strict_json_audit': False,
           'requested_inprocess_strict_json_qualified_numerator': None, 'denominator': 340,
           'new_guest_executions': 0, 'historical_route': 'old LiteInst CLI; owner architecture audit identifies mandatory ptrace host-hybrid, not requested in-process path',
           'linkage_counts': linkage_counts, 'linkage_scope': 'explicit pinned manifest metadata, not inspection of built ELF files',
           'target_90_percent': 306, 'allowed_nonpassing_cells_at_90_percent': 34,
           'known_static_deferral_ceiling_numerator': 336,
           'static_deferral_conclusion': '4 known static does not itself preclude 306/340; 336 unknown linkage means feasibility is not proven',
           'matrix': {'path': 'ci/compat-envelope/cells.json', 'sha256': digest(matrix_raw),
                      'git_blob': git('rev-parse', f'{SHA}:ci/compat-envelope/cells.json').decode().strip(),
                      'projection': matrix['projection']},
           'manifest_sources': manifest_sources,
           'sampling_input_sha256': digest((HERE / 'sampling-input.json').read_bytes()),
           'generator_sha256': digest(Path(__file__).read_bytes())}
write_json('baseline.json', {'summary': summary, 'rows': rows})
write_json('sample.json', {'seed': SEED, 'algorithm': 'ascending SHA256(UTF8(seed + NUL + identity)), identity tie-break; first 3 eligible',
                         'identity_fields': ['lane', 'category', 'test', 'mode', 'backend'],
                         'pool': 'all 25 historical liteinst verify red labels, then eligibility filter',
                         'eligibility': 'paired full-reference member AND cached-evidence subset AND C guest without explicit static flag AND historical ptrace max wall_time_max_ms <= 3000',
                         'linkage_warning': 'sampling heuristic does not establish dynamic linkage',
                         'candidates': candidates, 'selected': sample,
                         'raw_sources': sample_input['raw_sources'],
                         'failure_evidence': sample_input['failure_evidence'],
                         'interpretation': 'historically demoted candidates, not reproduced current in-process failures; retained sample rows pass-only'})
print(json.dumps({key: summary[key] for key in ('reference_count', 'historical_liteinst_labels', 'cached_evidence_subset_count', 'linkage_counts', 'population_sha256')}))
