from __future__ import annotations

import importlib.util
import pathlib
import sys
import unittest


SCRIPT = pathlib.Path(__file__).with_name("lint-rust-error-string-proxies.py")
SPEC = importlib.util.spec_from_file_location("rust_error_string_lint", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
LINT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LINT
SPEC.loader.exec_module(LINT)


class RustErrorStringProxyLintTest(unittest.TestCase):
    def kinds(self, source: str) -> list[str]:
        return [finding.kind for finding in LINT.scan_source(source)]

    def test_rejects_error_display_string_equality(self) -> None:
        source = """
fn classify(error: Errno) -> bool {
    error.to_string() == Errno::EFAULT.to_string()
}
"""
        self.assertEqual(
            self.kinds(source),
            ["error display string compared instead of typed error"],
        )

    def test_rejects_error_display_string_predicate_in_condition(self) -> None:
        source = """
fn retry(error: Error) {
    if error.to_string().contains("temporary") {
        retry_once();
    }
}
"""
        self.assertEqual(
            self.kinds(source),
            ["error display string used as a control-flow condition"],
        )

    def test_rejects_multiline_match_scrutinee(self) -> None:
        source = """
fn classify(operation_error: Error) {
    match operation_error
        .to_string()
        .as_str()
    {
        "missing" => recover(),
        _ => fail(),
    }
}
"""
        self.assertEqual(
            self.kinds(source),
            ["error display string used as a control-flow condition"],
        )

    def test_allows_typed_error_match(self) -> None:
        source = """
fn classify(error: Errno) -> bool {
    matches!(error, Errno::EFAULT)
}
"""
        self.assertEqual(self.kinds(source), [])

    def test_allows_error_rendering_outside_control_flow(self) -> None:
        source = """
fn report(error: Error) -> String {
    error.to_string()
}
"""
        self.assertEqual(self.kinds(source), [])

    def test_allows_map_err_conversion_inside_boolean_condition(self) -> None:
        source = """
fn query(image: Image) -> Result<(), String> {
    if !image.query().map_err(|error| error.to_string())? {
        return Err("query failed".into());
    }
    Ok(())
}
"""
        self.assertEqual(self.kinds(source), [])

    def test_allows_map_err_conversion_inside_match_scrutinee(self) -> None:
        source = """
fn run(builder: Builder) -> i32 {
    match builder.build().and_then(|runtime| {
        runtime.run().map_err(|error| std::io::Error::other(error.to_string()))
    }) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}
"""
        self.assertEqual(self.kinds(source), [])

    def test_map_err_exemption_ignores_the_binding_name(self) -> None:
        """A pure rename must not change the verdict.

        BOTH exemption tests above spell the binding ``|error|``, which is the
        only reason this went unnoticed: the rule accepted err, error, and
        anything ending _err or _error, but not ``e`` -- the most ordinary Rust
        name for a bound error. Measured before the fix, these five
        byte-equivalent snippets split 3 reported / 2 clean purely on spelling:
        e, ex and problem were reported, err and error were not. Renaming a
        closure variable turned a clean file red.

        That is what produced every finding the gate had against
        hermit/ci/manifest-plan/src/runner.rs. None of them compares an error
        string; the ``?`` propagates and the branch is on Some/None or on a
        bool. A submodule that had done nothing wrong reddened the parent.
        """
        template = """
fn walk(entry: DirEntry) -> Result<(), String> {{
    if entry.file_type().map_err(|{name}| {name}.to_string())?.is_dir() {{
        recurse()?;
    }}
    Ok(())
}}
"""
        for name in ("e", "err", "error", "ex", "problem"):
            with self.subTest(binding=name):
                self.assertEqual(self.kinds(template.format(name=name)), [])

    def test_map_err_exemption_still_reports_a_foreign_rendering(self) -> None:
        """The exemption is for converting the BOUND error, not any rendering.

        Widening it to "there is a map_err somewhere" would have been the easy
        fix and would have blinded the gate inside every map_err closure. The
        binding must be the thing rendered.
        """
        source = """
fn walk(entry: DirEntry) -> Result<(), String> {
    if entry.file_type().map_err(|e| other.to_string())?.is_dir() {
        recurse()?;
    }
    Ok(())
}
"""
        self.assertEqual(
            self.kinds(source),
            ["error display string used as a control-flow condition"],
        )

    def test_ignores_non_error_values_comments_and_strings(self) -> None:
        source = r'''
fn inspect(path: PathBuf) -> bool {
    // if error.to_string() == Errno::EFAULT.to_string() {}
    let example = "if error.to_string().contains(\"EFAULT\") {}";
    if path.to_string_lossy().contains("target") {
        return true;
    }
    !example.is_empty()
}
'''
        self.assertEqual(self.kinds(source), [])


if __name__ == "__main__":
    unittest.main()
