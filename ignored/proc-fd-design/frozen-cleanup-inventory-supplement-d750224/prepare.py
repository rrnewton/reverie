from pathlib import Path
import ast
import datetime
import difflib
import hashlib
import json
import os
import shutil
import stat
import subprocess

p = Path(__file__).resolve().parent
old = p.parent / 'frozen-cleanup-pair-review-1145835f'
own_review = p.parent / 'frozen-cleanup-inventory-review-d750224'
cleanup = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-frozen-cleanup-20260917')
au = cleanup / 'hermit/agent-utils'
evidence = cleanup / 'ignored/frozen-cleanup'
root_review = Path('/home/newton/work/dev-hermit/worktrees/slots/kvm-parent-reader-support-20260916/ignored/frozen-cleanup-inventory-review')
base = '1145835fc804f47ae48b29b89009e6937184175a'
head = 'd750224cbf10c5a3b35eec40fa4d57d0e98493cb'
tree = '8a41f1130199bc2dde1f60e6d053f1817375c94a'
prior_base = '8720799f78f8ef56d796723dbc473a9171a4f4eb'
parent_head = 'fba5d50c96756889bc5c2d3e6caffb9adc8bcb7a'
git_env = {**os.environ, 'GIT_NO_LAZY_FETCH': '1', 'GIT_OPTIONAL_LOCKS': '0'}
changed_path = 'py/tests/test_packaging_infrastructure.py'

def sha(data):
    return hashlib.sha256(data).hexdigest()

def put(name, data):
    q = p / name
    q.parent.mkdir(parents=True, exist_ok=True)
    with q.open('xb') as f:
        f.write(data.encode() if isinstance(data, str) else data)
    return q

def dump(name, obj):
    return put(name, json.dumps(obj, indent=2) + '\n')

def row(q):
    b = q.read_bytes()
    return {'path': str(q), 'bytes': len(b), 'sha256': sha(b),
            'mode': oct(stat.S_IMODE(q.stat().st_mode))}

def git(*args, repository=au):
    return subprocess.check_output(['git', *args], cwd=repository, env=git_env)

def tree_map(revision):
    result = {}
    for line in git('ls-tree', '-rz', revision).split(b'\0'):
        if not line:
            continue
        header, path = line.decode().split('\t', 1)
        mode, kind, blob = header.split()
        result[path] = {'mode': mode, 'kind': kind, 'object': blob}
    return result

assert git('rev-parse', head + '^{tree}').decode().strip() == tree
assert git('rev-list', '--parents', '-n', '1', head).decode().split() == [head, base]
before_map, after_map = tree_map(base), tree_map(head)
assert set(before_map) == set(after_map)
actual_changes = sorted(k for k in before_map if before_map[k] != after_map[k])
assert actual_changes == [changed_path]
patch = git('diff', '--binary', base, head)
assert sha(patch) == 'cd9bf13efb96c3cafa2c12e2c88cef1190fd8843b9138ccd0ffe682af37739b0'
assert patch == (evidence / 'au-inventory-correction-test/change.patch').read_bytes()
put('change.patch', patch)
put('commit.txt', git('cat-file', 'commit', head))
put('base-tree.txt', git('ls-tree', '-r', base))
put('head-tree.txt', git('ls-tree', '-r', head))

source_rows = []
def copy_source(revision, src, dst):
    raw = git('ls-tree', revision, '--', src).decode().rstrip('\n')
    header, actual = raw.split('\t', 1)
    mode, kind, blob = header.split()
    assert actual == src and kind == 'blob'
    data = git('cat-file', 'blob', blob)
    q = put('source/' + dst, data)
    q.chmod(0o755 if mode == '100755' else 0o644)
    source_rows.append({**row(q), 'path': dst, 'repository': str(au),
                        'revision': revision, 'git_path': src, 'git_blob': blob,
                        'git_mode': mode})
    return data

for path in [changed_path, 'Makefile', 'py/pyproject.toml',
             'py/wrkslots/tests/conftest.py', 'py/tests/conftest.py',
             'py/wrkslots/tests/test_lifecycle.py', 'py/wrkslots/cli.py', 'AGENTS.md']:
    copy_source(head, path, 'agent-utils/' + path)
before_packaging = copy_source(base, changed_path, 'base/' + changed_path)
prior_lifecycle = copy_source(prior_base, 'py/wrkslots/tests/test_lifecycle.py',
                              'prior-base/py/wrkslots/tests/test_lifecycle.py')
after_packaging = (p / 'source/agent-utils' / changed_path).read_bytes()
new_lifecycle = (p / 'source/agent-utils/py/wrkslots/tests/test_lifecycle.py').read_bytes()
assert new_lifecycle.startswith(prior_lifecycle)
for path in ('py/wrkslots/cli.py', 'py/wrkslots/tests/test_lifecycle.py'):
    assert before_map[path] == after_map[path]

copied = []
def copy_evidence(q, name, expected=None):
    data = q.read_bytes()
    if expected:
        assert sha(data) == expected, str(q)
    dest = put('evidence/' + name, data)
    copied.append({'original': str(q), **row(dest)})
    return dest

for n in ('REVIEW.md', 'RESULTS.md', 'COMPLETION-READBACK.json', 'exit.json',
          'input-binding.json', 'candidate-binding.json'):
    copy_evidence(old / n, 'prior-combined/' + n,
                  'b4bb147c3ad525d26962745beca5fc90fea0bb311c0ef7a96fe02c6cb5beeaae' if n == 'REVIEW.md' else None)
for n in ('RESULT.md', 'BINDING.json', 'AU-VALIDATE-FAILURE-MANIFEST.json',
          'RESULT.json', 'output.txt'):
    source = old / 'post-freeze-validation' / n
    if source.is_file():
        copy_evidence(source, 'failed-114-validation/' + n)
    else:
        fallback = evidence / ('au-make-validate/' + n if n in ('RESULT.json','output.txt') else n)
        copy_evidence(fallback, 'failed-114-validation/' + n)

inventory = evidence / 'au-lifecycle-inventory'
for q in sorted(inventory.iterdir()):
    if q.is_file() and q.suffix in ('.json', '.txt'):
        copy_evidence(q, 'collections/' + q.name)
for n in ('INPUT.json', 'RESULT.json', 'output.txt', 'time.txt', 'exit-code.txt'):
    copy_evidence(evidence / 'au-inventory-correction-test' / n, 'direct-control/' + n)
copy_evidence(evidence / 'AU-INVENTORY-COMMIT-READBACK.json', 'AU-INVENTORY-COMMIT-READBACK.json')
for n in ('REVIEW.md', 'READBACK.json'):
    copy_evidence(own_review / n, 'native-worker/' + n)
    copy_evidence(root_review / n, 'native-root/' + n,
                  'b5a834c6b27e2d90474dbef609b89c496199257d508f16bdb4dc9923b083b8cf' if n == 'REVIEW.md' else
                  '4152687093fc1735be4eb4db4ad7cc14ada846d5ab4b21194f036808c8772d99')

inventories = {}
collection_scalars = []
for phase, revision in [('before', prior_base), ('after', base)]:
    inv = json.loads((inventory / (phase + '-INVENTORY.json')).read_text())
    inventories[phase] = {k: set(v) for k, v in inv.items()}
    for part in ('all', 'ordinary', 'mapped', 'mapped_root'):
        receipt = json.loads((inventory / (phase + '-' + part + '.RESULT.json')).read_text())
        out = (inventory / (phase + '-' + part + '.stdout.txt')).read_bytes()
        err = (inventory / (phase + '-' + part + '.stderr.txt')).read_bytes()
        assert receipt['exit_code'] == 0 and receipt['revision'] == revision
        assert sha(out) == receipt['stdout_sha256'] and sha(err) == receipt['stderr_sha256']
        nodes = {line for line in out.decode().splitlines()
                 if line.startswith('wrkslots/tests/test_lifecycle.py::')}
        assert nodes == inventories[phase][part]
        collection_scalars.append({'phase': phase, 'partition': part, 'revision': revision,
                                   'collected': len(nodes), 'actual_exit': 0,
                                   'wall_seconds': receipt['seconds'],
                                   'stdout_sha256': sha(out), 'stderr_sha256': sha(err)})
    assert inventories[phase]['ordinary'].isdisjoint(inventories[phase]['mapped'])
    assert inventories[phase]['ordinary'] | inventories[phase]['mapped'] == inventories[phase]['all']
    assert inventories[phase]['mapped_root'] <= inventories[phase]['mapped']

before, after = inventories['before'], inventories['after']
assert {k: len(v) for k,v in before.items()} == {'all':654, 'ordinary':98, 'mapped':556, 'mapped_root':2}
assert {k: len(v) for k,v in after.items()} == {'all':797, 'ordinary':241, 'mapped':556, 'mapped_root':2}
assert all(before[k] <= after[k] for k in before)
assert before['mapped'] == after['mapped'] and before['mapped_root'] == after['mapped_root']
added = after['all'] - before['all']
assert len(added) == 143 and added == after['ordinary'] - before['ordinary']
methods = sorted({n.split('::',1)[1].split('[',1)[0] for n in added})
assert len(methods) == 8
proof = json.loads((inventory / 'PROOF.json').read_text())
assert sha((inventory/'PROOF.json').read_bytes()) == 'cec98ff6fc0a04d57241feff5f456de54787c979dd3685dc843902e2a59205b3'
assert methods == sorted(proof['added_test_methods'])

old_ast = ast.parse(before_packaging)
new_ast = ast.parse(after_packaging)
target = next(n for n in new_ast.body if isinstance(n, ast.FunctionDef) and n.name == 'test_wrkslots_lifecycle_partitions_are_disjoint_and_complete')
restored_counts, removed_names = [], []
for node in ast.walk(target):
    if isinstance(node, ast.Constant) and type(node.value) is int and node.value in (797,241):
        restored_counts.append(node.value)
        node.value = {797:654,241:98}[node.value]
    if isinstance(node, ast.Set):
        remove = [v for v in node.elts if isinstance(v, ast.Constant) and v.value in methods]
        removed_names.extend(v.value for v in remove)
        node.elts = [v for v in node.elts if v not in remove]
assert sorted(restored_counts) == [241,797] and sorted(removed_names) == methods
assert ast.dump(new_ast, include_attributes=False) == ast.dump(old_ast, include_attributes=False)
dump('COMPOSITION-SUMMARY.json', {
    'base':base,'head':head,'tree':tree,'sole_parent':base,
    'all_tree_entry_paths_identical':True,'tree_entry_count':len(after_map),
    'changed_paths':actual_changes,'every_other_blob_type_mode_unchanged':True,
    'retention_cli_and_lifecycle_test_identical_to_approved_114':True,
    'parent_head_unchanged':parent_head,
    'whole_packaging_ast_identical_after_exact_two_counts_and_eight_names_restored':True,
    'old_lifecycle_source_is_exact_prefix':True,
    'before_counts':{k:len(v) for k,v in before.items()},
    'after_counts':{k:len(v) for k,v in after.items()},
    'all654_prior_identities_preserved':True,'exact143_added_ordinary_cases':True,
    'new_method_names':methods,'collection_receipts':collection_scalars,
    'assertion_test_source_sha256':sha(after_packaging),
    'measurements':'Retained collections and direct test only; no new test/collection was executed during preparation.'})
dump('EVIDENCE-MANIFEST.json', copied)

prior = json.loads((old / 'candidate-binding.json').read_text())
repositories = {
    'parent':prior['repositories']['parent'],
    'agent-utils': {'repository':str(au),'base':base,'head':head,'tree':tree,
                    'changed':[changed_path],'patch_sha256':sha(patch)}}
candidate = {'root':str(p/'source'), 'base':base, 'head':head, 'tree':tree,
             'files':source_rows,'repositories':repositories,
             'candidate_patch_sha256':sha(patch),
             'prior_approved_pair':prior['repositories'],
             'prior_literal_review_sha256':sha((old/'REVIEW.md').read_bytes())}
dump('candidate-binding.json', candidate)

goal = '''GOALPOST-MOVING REVIEW RULE
Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.
Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it
'''
prompt = f'''Perform a bounded independent adversarial supplemental source review of the exact Agent Utils inventory-only commit {head}, tree {tree}, sole parent {base}. This follows a completed combined cleanup review; it is not a request to redo that full semantic review. Do not infer approval from the prior result: independently review the complete new delta and its source composition, then return APPROVE or CHANGES REQUESTED at this exact head.

Use only Read, Grep and Glob. No Bash, subprocesses, tests, compilation, collection, network, Git operations, source/index/ref/registry edits, cleanup, admission or public actions. Work from immutable copies and retained evidence rooted at {p}. Existing full prior source remains available under {old}, with its frozen input binding. Do not read mutable live source. The wrapper allows one 900-second run with 10-second termination grace and 64 MiB per output; a timeout or error is no approval.

Read these complete compact inputs first:
1. evidence/prior-combined/REVIEW.md: literal prior APPROVE for AU {base} and parent {parent_head}; SHA b4bb147c3ad525d26962745beca5fc90fea0bb311c0ef7a96fe02c6cb5beeaae. It remains unedited. The earlier source judgement and its limits are evidence, not an instruction to approve.
2. change.patch: all 1,830 bytes of the only new two-hunk delta; SHA cd9bf13efb96c3cafa2c12e2c88cef1190fd8843b9138ccd0ffe682af37739b0. Read the full before and after py/tests/test_packaging_infrastructure.py under source/base and source/agent-utils, including the actual subprocess collection helper and all assertions in the changed method.
3. COMPOSITION-SUMMARY.json, candidate-binding.json and EVIDENCE.md. Full base-tree.txt/head-tree.txt are retained for exact entry mapping, but do not dump every entry into the answer. The preparation independently compared their complete path/type/mode/blob mappings: exactly this one test file differs. The CLI retention implementation and lifecycle test are byte/mode-identical to 114; parent fba5 is unchanged.
4. evidence/failed-114-validation/output.txt and RESULT.json: the actual original normal validation failure, not a passing run. Read the direct-control output/receipt separately.
5. evidence/collections/PROOF.json plus before/after inventory and raw collection receipts as needed to authenticate actual identities and partitions. A compact scalar inventory is in COMPOSITION-SUMMARY.json. All eight original collection outputs and actual commands/exit statuses are retained; no collection is being executed by this review.

The only intended delta raises exact total654 to797 and exact ordinary98 to241 and adds eight existing preservation method names to the exact expected set. No tests are added in this commit. Those eight methods and their 143 parameterized cases were appended in the already-reviewed 114 cleanup change. All654 prior collected identities, mapped556 and both exact mapped-root identities must survive; the new143 must be exactly ordinary-environment cases of those eight methods. Check disjointness, exhaustive union, root membership, special-case membership and actual Makefile invocation assertions, as well as all original method names. Do not accept a count increase by itself as proof. Read py/wrkslots/tests/conftest.py and py/pyproject.toml and the relevant Makefile paths to verify the actual partitioning contract. The existing full lifecycle test is available for marker/parameter source checks; the prior872 lifecycle file is supplied to check the exact old-source prefix. No need to repeat the unchanged preservation implementation audit unless this delta creates a concrete interaction.

The ordinary test process launches successful strict-marker pytest collections with explicit marker expressions; the mapped execution still uses the existing user/PID namespace route. Check there is no hidden selector, skip, allowlist, fallback, weakened comparator or case deletion. Verify all143 new identities have a real selected population, rather than merely being named in prose. Independent native reviews are supplementary and are not authority to skip this verification.

Evidence must remain attributed exactly. The frozen prior review correctly approved source but its normal-validation status was pending at its prompt cutoff. Later normal validation on114 failed with exit2 after587.73 seconds: first partition2,967 passes and one failing stale inventory assertion; later lifecycle/Rust-test/cross/package stages were not reached. Preserve this failure. The directly affected inventory assertion passed once on the new file bytes before the d750 commit: one pass in1.98 pytest seconds,2.288905579 outer wall seconds; INPUT.json names base114 plus the exact new file hash, so do not relabel it as a post-commit whole-suite run. Those bytes are exactly committed atd750. A separate normal make validate atd750 is running/pending at this package cutoff; no full-validation pass exists in these frozen inputs.

Prior evidence remains43 focused AU cases plus strict mypy13 files;43 includes existing controls and is not a claim that all new cases ran. Parent had28 passing methods plus four fixture-setup failures in the first32-method attempt; corrected four passed in a separate attempt with production unchanged, yielding32 distinct selected passing methods across two attempts. The prior report's approximate '~145' new-case language is superseded by the actual143 collection; its shape tuple has15 mutations, not16. Preserve the literal prior report and use exact evidence in your supplement.

The supplement must not imply real1839 eligibility, present liveness, cleanup, deletion, scorecard handoff, test qualification, or pair activation. The prior semantic conclusion remains preservation-only: direct recovery still requires four proof artifacts. Root now reports the parent slice landed separately as55d1426b after its independently reviewed incoming-main composition; that later parent composition is outside this inventory-only review, whose prior reference stays the literal fba5 review. Root also reports the first partition of the new d750 normal run passed2,968 cases and later lifecycle checks are active; no terminal full-validation receipt is supplied here. No existing validation requirement is removed; do not invent an exact-head whole-DAG or whole-parent-suite receipt requirement merely to review this unchanged AU implementation plus inventory correction.

Return a concise complete report: exact source identity; concrete findings with file/line, consequence and correction if any; explicit goalpost assessment; actual inspected controls and evidence limits; and verdict on this exact inventory delta and its composition with the already-approved AU114/parentfba5 pair. If a claimed guard is absent, explain the actual path rather than hypothesizing. Do not instruct implementation, execute tests or change anything.

{goal}'''
put('prompt.txt', prompt)
put('EVIDENCE.md', f'''# Inventory-only followup evidence

Target AU `{base}..{head}`, tree `{tree}`. Parent remains `{parent_head}`. Only `py/tests/test_packaging_infrastructure.py` changes. All production and prior lifecycle test blobs/modes remain identical; the complete original semantic verdict is retained unedited in `evidence/prior-combined/REVIEW.md`.

Retained collections (no new execution during preparation):

| Source | All | Ordinary | Mapped | Mapped root |
| --- | ---: | ---: | ---: | ---: |
| 8720799f | 654 | 98 | 556 | 2 |
| 1145835f | 797 | 241 | 556 | 2 |

All654 prior identities survive. Exactly143 added identities come from eight appended ordinary-environment methods. Mapped556 and both mapped-root identities are identical. The old lifecycle file is an exact byte prefix of the new one. The source atd750 preserves the entire114 lifecycle file. All eight collection processes returned0; raw node lists, commands, durations, source archive bindings and stderr are retained under `evidence/collections/`. `COMPOSITION-SUMMARY.json` records independent set and whole-file AST comparisons; both native reviewer readbacks are retained as supplementary evidence.

The original normal114 run failed (exit2,587.73wall): first Python partition2,967 passed/1failed,2warnings in521.47s. The stale654 count assertion failed at actual797; preceding partition-disjointness/union assertions passed. Later lifecycle/Rust-test/cross/package stages were not reached. This failure is unchanged. The new inventory assertion alone then passed once,1.98pytest seconds/2.288905579 outer wall, on the exact new file bytes before commitd750. Its input explicitly binds base114 plus new source SHA281efe49. It is not a post-commit full-validation result. Normald750 validation is pending at preparation cutoff; do not infer its eventual result.

The prior combined review returned actual literalAPPROVE, exit0/762.907865s, all80inputs and25sourcecopies unchanged, Read/Grep/Glob only. AU43focused and mypy13passed; parent28passes then corrected4passes are separate retained attempts. No full parent suite, real1839 census/admission, removal proof, scorecard completion or cleanup is claimed. Prior approximate new-case counts are corrected to exact143/eightmethods here, without editing the original report or deleting failures.

Root's later status message reports first-partition2,968 passes in the incomplete d750 normal run and active lifecycle checks. It also reports a separate parent landing at55d1426b after native composition review. Neither is recast here as a new complete validation receipt or an independently reviewed parent composition; this supplement remains only the d750 inventory delta against the literal prior fba5/114 verdict.

No product source, refs, registry or public state was changed by this preparation. No collection, build, test, guest or cleanup was executed. This package is unexecuted pending root release. All later validation results must be appended separately without mutating frozen inputs.
''')

launcher_old = (old / 'launch-review.py').read_text()
launcher = launcher_old.replace('--execute-reviewed-1145835f-fba5d50c', '--execute-reviewed-d750224c-inventory')
launcher = launcher.replace("str(Path(source['root']) / 'parent')", "str(Path(source['root']) / 'agent-utils')")
put('launch-review.py', launcher)
put('launcher.diff', ''.join(difflib.unified_diff(launcher_old.splitlines(True), launcher.splitlines(True),
    fromfile=str(old/'launch-review.py'), tofile=str(p/'launch-review.py'))))
plan = json.loads((old / 'execution-plan.json').read_text())
plan.update(state='prepared_unexecuted_pending_root_release',
            proposed_released_argv=['/usr/bin/python3','-B',str(p/'launch-review.py'),'--execute-reviewed-d750224c-inventory'],
            cwd=str(p/'source/agent-utils'),
            candidate_binding_sha256=sha((p/'candidate-binding.json').read_bytes()),
            outputs=[str(p/n) for n in ('launch.json','stdout.jsonl','stderr.log','exit.json')],
            source_scope='Only the complete AU114..d750 inventory assertion delta and its unchanged composition with approved114/fba5. Prior literal source verdict retained; no duplicate full semantic review.',
            repositories=repositories,
            actual_evidence_cutoff='Prior combined review APPROVE; original114 normal validation exit2/2967pass1fail retained. Direct updated inventory assertion1pass before commit, exact committed bytes. Normald750 validation pending; no full-suite or real-cleanup claim.')
dump('execution-plan.json', plan)
put('plan.diff', ''.join(difflib.unified_diff((old/'execution-plan.json').read_text().splitlines(True),
    (p/'execution-plan.json').read_text().splitlines(True),fromfile=str(old/'execution-plan.json'),tofile=str(p/'execution-plan.json'))))

# Include the exact old frozen input set for optional unchanged context, not its
# raw private transcript. The new literal verdict and audit carry public results.
old_inputs = json.loads((old/'input-binding.json').read_text())
assert len(old_inputs) == 80
inputs = {r['path']:r for r in old_inputs}
for expected in old_inputs:
    assert row(Path(expected['path'])) == expected
for q in sorted(p.rglob('*')):
    if q.is_file():
        inputs[str(q)] = row(q)
for tool in plan['tools_current_metadata']:
    assert row(Path(tool['path'])) == {k:tool[k] for k in ('path','bytes','sha256','mode')}
dump('input-binding.json', sorted(inputs.values(), key=lambda r:r['path']))
dump('PREPARATION-READBACK.json', {
    'created_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'state':'prepared_not_executed','head':head,'base':base,'tree':tree,
    'all_inputs_verified':True,'input_count':len(inputs),'new_source_copy_count':len(source_rows),
    'prior80_frozen_input_paths_preserved':True,'live_source_HEAD_or_packaging_paths_are_not_inputs':True,
    'raw_private_transcript_not_input':True,'source_and_collection_verification_only_no_tests':True,
    'files':[row(p/n) for n in ('prompt.txt','launch-review.py','launcher.diff','execution-plan.json','plan.diff',
                              'input-binding.json','candidate-binding.json','change.patch','EVIDENCE.md',
                              'COMPOSITION-SUMMARY.json')]})
print(json.dumps({'head':head,'source_copies':len(source_rows),'inputs':len(inputs),
                  'prompt_bytes':len(prompt.encode()),'state':'prepared_not_executed',
                  'binding_sha256':sha((p/'input-binding.json').read_bytes())},indent=2))
