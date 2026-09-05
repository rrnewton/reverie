"""Static guard: production ledger consumers use one shared boundary."""

import ast
import symtable
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
CI_HUB = ROOT / "ci-hub"

RETIRED_TOKENS = (
    "validate-run-" + "ledger.jsonl",
    "HERMIT_" + "VALIDATE_LEDGER",
    "REVERIE_" + "VALIDATE_LEDGER",
    "DISCOVERED parent " + "ledger",
    "ci/validate-" + "ledger",
)

# Every production module that consumes receipt rows. Each must either import
# the canonical adapter directly or invoke the Rust command whose default loader
# delegates to that adapter.
CONSUMERS = (
    "health/pr_status.py",
    "history/query.py",
    "history/validate_time_ratchet.py",
    "lib/validate_status.rs",
    "validate/aggregate.py",
    "validate/anchor_select.py",
    "validate/attribute_reds.py",
    "validate/backend_factor.py",
    "validate/failure_evidence.py",
    "validate/finalize_receipt.py",
    "validate/green_class.py",
    "validate/mutation_suite.py",
    "validate/qualified_rows.py",
    "validate/start_unit.py",
    "validate/totality.py",
    "validate/wall_cpu_ratchet.py",
    "main-metrics/wall_series.py",
)


def _root_relative_str(node: ast.AST, relative: str) -> bool:
    if not (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Name)
        and node.func.id == "str"
        and len(node.args) == 1
    ):
        return False
    path = node.args[0]
    return (
        isinstance(path, ast.BinOp)
        and isinstance(path.op, ast.Div)
        and isinstance(path.left, ast.Name)
        and path.left.id == "root"
        and isinstance(path.right, ast.Constant)
        and path.right.value == relative
    )


def _sequence_variants(
    node: ast.AST,
    assignments: dict[str, list[ast.AST]],
    seen: frozenset[str] = frozenset(),
) -> list[list[ast.AST]]:
    if isinstance(node, (ast.List, ast.Tuple)):
        return [list(node.elts)]
    if isinstance(node, ast.Name) and node.id not in seen:
        return [
            variant
            for assigned in assignments.get(node.id, [])
            for variant in _sequence_variants(
                assigned, assignments, seen | {node.id}
            )
        ]
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return [
            left + right
            for left in _sequence_variants(node.left, assignments, seen)
            for right in _sequence_variants(node.right, assignments, seen)
        ]
    return []


def _runner_reference(
    node: ast.AST,
    assignments: dict[str, list[ast.AST]],
    runner_names: set[str],
    subprocess_names: set[str],
    seen: frozenset[str] = frozenset(),
) -> bool:
    if isinstance(node, ast.Name) and node.id in runner_names:
        return True
    if (
        isinstance(node, ast.Attribute)
        and isinstance(node.value, ast.Name)
        and node.value.id in subprocess_names
        and node.attr == "run"
    ):
        return True
    if isinstance(node, ast.Name) and node.id not in seen:
        if any(
            _runner_reference(
                value,
                assignments,
                runner_names,
                subprocess_names,
                seen | {node.id},
            )
            for value in assignments.get(node.id, [])
        ):
            return True
    if isinstance(node, ast.Call):
        partial = (
            isinstance(node.func, ast.Name)
            and node.func.id == "partial"
            or isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "functools"
            and node.func.attr == "partial"
        )
        return bool(node.args) and partial and _runner_reference(
            node.args[0], assignments, runner_names, subprocess_names, seen
        )
    if isinstance(node, ast.Subscript):
        return _runner_reference(
            node.value, assignments, runner_names, subprocess_names, seen
        )
    if isinstance(node, (ast.List, ast.Tuple)):
        return any(
            _runner_reference(
                child, assignments, runner_names, subprocess_names, seen
            )
            for child in node.elts
        )
    return False


def _runner_call(
    node: ast.Call,
    assignments: dict[str, list[ast.AST]],
    runner_names: set[str],
    subprocess_names: set[str],
) -> bool:
    return _runner_reference(
        node.func, assignments, runner_names, subprocess_names
    )


def _constant_values(
    node: ast.AST,
    assignments: dict[str, list[ast.AST]],
    seen: frozenset[str] = frozenset(),
) -> set[object]:
    if isinstance(node, ast.Constant):
        return {node.value}
    if isinstance(node, ast.Name) and node.id not in seen:
        return {
            value
            for assigned in assignments.get(node.id, [])
            for value in _constant_values(assigned, assignments, seen | {node.id})
        }
    return set()


def _dead_calls(tree: ast.AST) -> set[int]:
    dead: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.If) and isinstance(node.test, ast.Constant):
            branches = node.orelse if bool(node.test.value) else node.body
            for statement in branches:
                dead.update(
                    id(candidate)
                    for candidate in ast.walk(statement)
                    if isinstance(candidate, ast.Call)
                )
    return dead


def _conditional_calls(tree: ast.AST) -> set[int]:
    conditional: set[int] = set()
    for node in ast.walk(tree):
        if isinstance(
            node,
            (
                ast.If,
                ast.IfExp,
                ast.BoolOp,
                ast.For,
                ast.AsyncFor,
                ast.While,
                ast.Try,
                ast.Match,
                ast.With,
                ast.AsyncWith,
            ),
        ):
            conditional.update(
                id(candidate)
                for candidate in ast.walk(node)
                if isinstance(candidate, ast.Call)
            )
    return conditional


def _deferred_calls(tree: ast.AST) -> set[int]:
    deferred: set[int] = set()
    for node in ast.walk(tree):
        if node is tree:
            continue
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef)):
            deferred.update(
                id(candidate)
                for candidate in ast.walk(node)
                if isinstance(candidate, ast.Call)
            )
    return deferred


def _injected_runner_is_unmodified(text: str) -> bool:
    """Require the production function's injected runner to stay authoritative."""
    try:
        module = symtable.symtable(text, "<ledger-reader-allowlist>", "exec")
    except SyntaxError:
        return False
    functions = [
        child
        for child in module.get_children()
        if child.get_name() == "assert_row_readable_from_canonical_ledger"
    ]
    if len(functions) != 1:
        return False
    try:
        runner = functions[0].lookup("run")
    except KeyError:
        return False
    return (
        runner.is_parameter()
        and not runner.is_assigned()
        and not runner.is_imported()
    )


def invokes_canonical_row_boundary(text: str) -> bool:
    """Recognize complete canonical argv passed to the actual ``run`` call.

    ``start_unit`` needs all canonical rows, including nonpass rows, to bind a
    completed child to the verdict returned by ``validate-status``. Requiring
    the pass-only ``ledger qualified-rows`` spelling would reject that valid
    transition, but that prior form remains canonical too. Both forms require
    an exact three-element argv on the actual runner call. An unused example
    list, comment, string, ``echo`` command, or same-named script elsewhere is
    only decoration and must fail. Every row-boundary-shaped ``run`` call must
    be canonical, so one legitimate call cannot bless a second shadow reader.
    """
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return False
    production_functions = [
        node
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name == "assert_row_readable_from_canonical_ledger"
    ]
    if len(production_functions) > 1:
        return False
    if production_functions and not _injected_runner_is_unmodified(text):
        return False
    scope: ast.AST = production_functions[0] if len(production_functions) == 1 else tree
    runner_names = {"run"}
    subprocess_names = {"subprocess"}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module == "subprocess":
            runner_names.update(
                alias.asname or alias.name
                for alias in node.names
                if alias.name == "run"
            )
        elif isinstance(node, ast.Import):
            subprocess_names.update(
                alias.asname or alias.name
                for alias in node.names
                if alias.name == "subprocess"
            )

    assignments: dict[str, list[ast.AST]] = {}
    for node in ast.walk(scope):
        if (
            isinstance(node, ast.Assign)
            and len(node.targets) == 1
            and isinstance(node.targets[0], ast.Name)
        ):
            name = node.targets[0].id
            assignments.setdefault(name, []).append(node.value)
        elif (
            isinstance(node, ast.AnnAssign)
            and isinstance(node.target, ast.Name)
            and node.value is not None
        ):
            assignments.setdefault(node.target.id, []).append(node.value)

    allowed_calls: list[bool] = []
    canonical_row_calls = 0
    dead_calls = _dead_calls(scope)
    conditional_calls = _conditional_calls(scope)
    deferred_calls = _deferred_calls(scope)
    for node in ast.walk(scope):
        if not isinstance(node, ast.Call) or not _runner_call(
            node, assignments, runner_names, subprocess_names
        ):
            continue
        immediate = (
            id(node) not in dead_calls
            and id(node) not in deferred_calls
            and id(node) not in conditional_calls
        )
        argv_nodes = list(node.args[:1]) + [
            keyword.value for keyword in node.keywords if keyword.arg == "args"
        ]
        if len(argv_nodes) != 1:
            allowed_calls.append(False)
            continue
        variants = _sequence_variants(argv_nodes[0], assignments)
        literal_argv = isinstance(argv_nodes[0], (ast.List, ast.Tuple))
        direct_run = isinstance(node.func, ast.Name) and node.func.id == "run"
        call_allowed = immediate and literal_argv and direct_run and len(variants) == 1
        row_call = False
        status_call = False
        for argv in variants:
            if len(argv) < 3:
                continue
            direct_adapter = (
                len(argv) == 3
                and isinstance(argv[0], ast.Attribute)
                and isinstance(argv[0].value, ast.Name)
                and argv[0].value.id == "sys"
                and argv[0].attr == "executable"
                and _root_relative_str(argv[1], "ci-hub/ledger/validate_rows.py")
                and isinstance(argv[2], ast.Constant)
                and argv[2].value == "rows"
            )
            canonical_cli = (
                len(argv) == 3
                and _root_relative_str(argv[0], "ci-hub/ci-hub")
                and isinstance(argv[1], ast.Constant)
                and argv[1].value == "ledger"
                and isinstance(argv[2], ast.Constant)
                and argv[2].value == "qualified-rows"
            )
            row_call = row_call or direct_adapter or canonical_cli
            status_call = status_call or (
                len(argv) == 5
                and _root_relative_str(argv[0], "ci-hub/ci-hub")
                and "validate-status" in _constant_values(argv[1], assignments)
                and "--sha" in _constant_values(argv[2], assignments)
                and isinstance(argv[3], ast.Name)
                and argv[3].id == "target"
                and "--json" in _constant_values(argv[4], assignments)
            )
        call_allowed = call_allowed and (row_call or status_call)
        allowed_calls.append(call_allowed)
        if call_allowed and row_call:
            canonical_row_calls += 1
    return bool(allowed_calls) and all(allowed_calls) and canonical_row_calls == 1


def test_no_production_source_names_a_retired_ledger_location() -> None:
    offenders = []
    for path in CI_HUB.rglob("*"):
        if (
            not path.is_file()
            or "tests" in path.parts
            or path.name.startswith("test_")
            or path.suffix not in {".py", ".rs", ".sh"}
        ):
            continue
        text = path.read_text(errors="replace")
        found = [token for token in RETIRED_TOKENS if token in text]
        if found:
            offenders.append(f"{path.relative_to(ROOT)}: {found}")
    assert not offenders, "retired ledger locations remain:\n" + "\n".join(offenders)


def test_every_declared_consumer_delegates_to_the_canonical_boundary() -> None:
    missing = []
    for rel in CONSUMERS:
        text = (CI_HUB / rel).read_text()
        if rel == "lib/validate_status.rs":
            # Rust's loader lives in ci-hub.rs; the library declares only the
            # logical ledger root and never a physical receipt file.
            ok = 'pub const LEDGER_REL: &str = "ledger"' in text
        elif rel in {"validate/start_unit.py"}:
            ok = invokes_canonical_row_boundary(text)
        elif rel in {"history/validate_time_ratchet.py", "main-metrics/wall_series.py"}:
            ok = "qualified" in text and "DEFAULT_LEDGER" in text
        else:
            ok = "canonical_ledger" in text or "qualified-rows" in text
        if not ok:
            missing.append(rel)
    assert not missing, "consumer bypasses canonical ledger adapter: " + ", ".join(missing)


def test_start_unit_exact_canonical_row_adapter_is_accepted() -> None:
    planted = '''
rows = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert invokes_canonical_row_boundary(planted)


def test_start_unit_prior_canonical_cli_is_still_accepted() -> None:
    planted = '''
rows = run(
    [str(root / "ci-hub/ci-hub"), "ledger", "qualified-rows"],
    check=False,
)
'''
    assert invokes_canonical_row_boundary(planted)


def test_injected_runner_parameter_is_still_accepted() -> None:
    planted = '''
def assert_row_readable_from_canonical_ledger(root, target, *, run):
    rows = run(
        [str(root / "ci-hub/ci-hub"), "ledger", "qualified-rows"],
        check=False,
    )
'''
    assert invokes_canonical_row_boundary(planted)


def test_nested_runner_parameter_does_not_rebind_the_production_runner() -> None:
    planted = '''
def assert_row_readable_from_canonical_ledger(root, target, *, run):
    def helper(run):
        return run

    rows = run(
        [str(root / "ci-hub/ci-hub"), "ledger", "qualified-rows"],
        check=False,
    )
'''
    assert invokes_canonical_row_boundary(planted)


def test_rebound_runner_cannot_qualify_canonical_argv() -> None:
    bindings = {
        "assignment": "    run = noop",
        "annotated assignment": "    run: object = noop",
        "chained assignment": "    run = other = noop",
        "unpacked assignment": "    run, other = (noop, None)",
        "named expression": "    (run := noop)",
        "for target": "    for run in [noop]:\n        pass",
        "with target": "    with manager as run:\n        pass",
        "import": "    from ignored import run",
        "function definition": (
            "    def run(*args, **kwargs):\n"
            "        return None"
        ),
        "match capture": "    match noop:\n        case run:\n            pass",
    }
    for description, binding in bindings.items():
        planted = f'''
def assert_row_readable_from_canonical_ledger(root, target, *, run):
    noop = lambda *args, **kwargs: None
{binding}
    rows = run(
        [str(root / "ci-hub/ci-hub"), "ledger", "qualified-rows"],
        check=False,
    )
'''
        assert not invokes_canonical_row_boundary(planted), description


def test_start_unit_shadow_row_adapter_is_rejected() -> None:
    planted = '''
# A decorative canonical path in prose must not bless the shadow command:
# str(root / "ci-hub/ledger/validate_rows.py"), "rows"
rows = run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_unused_canonical_argv_cannot_bless_shadow_adapter() -> None:
    planted = '''
CANONICAL_EXAMPLE = [
    sys.executable,
    str(root / "ci-hub/ledger/validate_rows.py"),
    "rows",
]
rows = run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_a_second_shadow_adapter() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_dead_canonical_call_cannot_bless_a_live_shadow_adapter() -> None:
    planted = '''
if False:
    run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
        check=False,
    )
rows = run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_dead_canonical_call_alone_is_not_a_reader() -> None:
    planted = '''
if False:
    run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
        check=False,
    )
'''
    assert not invokes_canonical_row_boundary(planted)


def test_conditional_canonical_call_alone_is_not_a_guaranteed_reader() -> None:
    planted = '''
if enabled:
    run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
        check=False,
    )
'''
    assert not invokes_canonical_row_boundary(planted)


def test_boolean_short_circuit_canonical_call_is_not_a_reader() -> None:
    planted = '''
False and run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_ternary_canonical_call_is_not_a_guaranteed_reader() -> None:
    planted = '''
rows = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
) if enabled else None
'''
    assert not invokes_canonical_row_boundary(planted)


def test_deferred_canonical_call_alone_is_not_a_reader() -> None:
    planted = '''
def maybe_read_later():
    return run(
        [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
        check=False,
    )
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_named_shadow_argv() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
shadow = [sys.executable, str(root / "ignored/validate_rows.py"), "rows"]
rows = run(shadow, check=False)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_annotated_shadow_argv() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
shadow: list[str] = [sys.executable, str(root / "ignored/validate_rows.py"), "rows"]
rows = run(shadow, check=False)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_reassigned_shadow_argv() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
shadow = ["echo", "not", "a-reader"]
shadow = [sys.executable, str(root / "ignored/validate_rows.py"), "rows"]
rows = run(shadow, check=False)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_indirect_verb_shadow() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
verb = "rows"
rows = run(
    [sys.executable, str(root / "ignored/validate_rows.py"), verb],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_reassigned_verb_cannot_qualify_as_literal_canonical_argv() -> None:
    planted = '''
verb = "rows"
verb = "shadow"
rows = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), verb],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_runner_alias_shadow() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
runner = run
rows = runner(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_subprocess_runner_alias() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
runner = subprocess.run
rows = runner(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_two_hop_runner_alias() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
runner = run
runner2 = runner
rows = runner2(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_partial_runner_alias() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
shadow_runner = functools.partial(
    run,
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
shadow = shadow_runner()
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_subscript_runner_alias() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
shadow = [run][0](
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_keyword_shadow_argv() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = subprocess.run(
    args=[sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_overlong_shadow_argv() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows", "--all"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_mutated_canonical_argv_is_not_owned_by_the_boundary() -> None:
    planted = '''
argv = [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"]
argv[1] = str(root / "ignored/validate_rows.py")
rows = run(argv, check=False)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_appended_row_verb_is_not_owned_by_the_boundary() -> None:
    planted = '''
shadow = [sys.executable, str(root / "ignored/validate_rows.py")]
shadow.append("rows")
rows = run(shadow, check=False)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_computed_appended_row_verb_is_not_a_boundary() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
shadow = [sys.executable, str(root / "ignored/validate_rows.py")]
shadow.append("ro" + "ws")
rows = run(shadow, check=False)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_shell_wrapped_shadow_is_not_an_allowed_runner_call() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = run(
    ["sh", "-c", "python ignored/validate_rows.py rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_unrelated_rows_label_does_not_invalidate_canonical_call() -> None:
    planted = '''
label = "rows"
rows = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert invokes_canonical_row_boundary(planted)


def test_imported_runner_cannot_bless_shadow_argv() -> None:
    planted = '''
from subprocess import run as runner
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = runner(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_module_alias_runner_cannot_bless_shadow_argv() -> None:
    planted = '''
import subprocess as sp
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = sp.run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_default_argument_shadow_executes_outside_owned_boundary() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
def later(
    value=run(
        [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
        check=False,
    )
):
    return value
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_unresolved_runner_argv() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = run(build_shadow_argv(), check=False)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_concatenated_shadow_argv() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"] + [],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_canonical_call_cannot_bless_direct_subprocess_shadow() -> None:
    planted = '''
canonical = run(
    [sys.executable, str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
rows = subprocess.run(
    [sys.executable, str(root / "ignored/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_echo_of_canonical_argv_is_not_a_ledger_reader() -> None:
    planted = '''
rows = run(
    ["echo", str(root / "ci-hub/ledger/validate_rows.py"), "rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_legacy_substring_decoys_cannot_bless_shadow_cli() -> None:
    planted = '''
# Decorative legacy tokens: "ledger", "qualified-rows"
CANONICAL_EXAMPLE = '"ledger", "qualified-rows"'
rows = run(
    [str(root / "ignored/ci-hub"), "ledger", "qualified-rows"],
    check=False,
)
'''
    assert not invokes_canonical_row_boundary(planted)


def test_rust_front_controller_executes_the_canonical_adapter() -> None:
    text = (CI_HUB / "ci-hub.rs").read_text()
    assert 'root.join("ci-hub/ledger/validate_rows.py")' in text
    assert '.arg("rows")' in text
