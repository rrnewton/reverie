// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Thin host for one checked, owned normal Rust wrapper action.
//! All recipe/expectation reads and qualification stay in the library factory.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;

use reverie_liteinst_artifact::native_action::OwnedProduct;
use reverie_liteinst_artifact::native_action::execute_owned_rust_action_files;
use reverie_liteinst_artifact::native_action_input::LeafOutputs;

#[derive(Debug, PartialEq, Eq)]
struct Arguments {
    expected: PathBuf,
    outputs: LeafOutputs,
    wrapper: Vec<String>,
}

fn arguments(args: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut args = args.into_iter();
    let mut paths = BTreeMap::new();
    let mut separated = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            separated = true;
            break;
        }
        let flag = arg.into_string().map_err(|_| "host option is not UTF-8")?;
        if !matches!(
            flag.as_str(),
            "--expected-inputs"
                | "--action-inputs"
                | "--output-directory"
                | "--invocation"
                | "--evidence"
                | "--finalization"
                | "--input-inventory"
                | "--qualified-product"
                | "--product"
        ) {
            return Err(format!(
                "unknown host option or positional argument: {flag}"
            ));
        }
        if paths.contains_key(&flag) {
            return Err(format!("duplicate host option: {flag}"));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing path for {flag}"))?;
        if value.is_empty() || value.as_bytes().contains(&0) || value.as_bytes().starts_with(b"--")
        {
            return Err(format!("missing or invalid path for {flag}"));
        }
        paths.insert(flag, PathBuf::from(value));
    }
    if !separated {
        return Err("missing -- before the whole normal wrapper command".into());
    }
    let wrapper = args
        .map(|arg| {
            let arg = arg
                .into_string()
                .map_err(|_| "wrapper argument is not UTF-8")?;
            if arg.contains('\0') {
                return Err("wrapper argument contains NUL".into());
            }
            Ok(arg)
        })
        .collect::<Result<Vec<_>, String>>()?;
    if wrapper.first().is_none_or(String::is_empty) {
        return Err("missing whole normal wrapper executable after --".into());
    }
    let mut required = |flag: &str| {
        paths
            .remove(flag)
            .ok_or_else(|| format!("missing required host option: {flag}"))
    };
    let expected = required("--expected-inputs")?;
    let recipe = required("--action-inputs")?;
    let directory = required("--output-directory")?;
    let invocation = required("--invocation")?;
    let evidence = required("--evidence")?;
    let finalization = required("--finalization")?;
    let input_inventory = required("--input-inventory")?;
    let product = required("--product")?;
    Ok(Arguments {
        expected,
        outputs: LeafOutputs {
            recipe,
            product,
            directory,
            invocation,
            evidence,
            finalization,
            input_inventory,
            qualified_product: paths.remove("--qualified-product"),
        },
        wrapper,
    })
}

fn run(args: Arguments) -> Result<(), String> {
    let result = execute_owned_rust_action_files(&args.expected, &args.outputs, args.wrapper)?;
    let (kind, compilation) = match &result {
        OwnedProduct::Compilation(compilation) => ("compilation", compilation),
        OwnedProduct::Constructor(qualified) => {
            ("constructor_qualification", qualified.compilation())
        }
        OwnedProduct::StandaloneConstructor(qualified) => (
            "standalone_constructor_qualification",
            qualified.compilation(),
        ),
        OwnedProduct::NativeEntry(qualified) => {
            ("native_entry_qualification", qualified.compilation())
        }
    };
    let output = serde_json::json!({
        "kind":kind, "product":compilation.product(), "evidence":compilation.evidence(), "reused":compilation.reused(),
    });
    writeln!(std::io::stdout().lock(), "{output}")
        .map_err(|e| format!("write completed action result: {e}"))
}

fn main() -> ExitCode {
    match arguments(std::env::args_os().skip(1)).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("reverie-native-rust-action: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn host() -> Vec<OsString> {
        [
            "--expected-inputs",
            "independent.json",
            "--action-inputs",
            "recipe.json",
            "--output-directory",
            "outputs",
            "--invocation",
            "outputs/invocation.json",
            "--evidence",
            "outputs/evidence.json",
            "--finalization",
            "outputs/finalization.json",
            "--input-inventory",
            "outputs/input_inventory.json",
            "--product",
            "product.rlib",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn wrapper_boundary_preserves_every_argument_without_shell_interpretation() {
        let mut input = host();
        input.extend(
            [
                "--qualified-product",
                "outputs/qualified/product",
                "--",
                "./wrapper",
                "--product",
                "a path with spaces",
                "",
                "$(literal)",
                "--",
            ]
            .map(OsString::from),
        );
        let parsed = arguments(input).unwrap();
        assert_eq!(parsed.expected, PathBuf::from("independent.json"));
        assert_eq!(parsed.outputs.recipe, PathBuf::from("recipe.json"));
        assert_eq!(parsed.outputs.product, PathBuf::from("product.rlib"));
        assert_eq!(parsed.outputs.directory, PathBuf::from("outputs"));
        assert_eq!(
            parsed.outputs.invocation,
            PathBuf::from("outputs/invocation.json")
        );
        assert_eq!(
            parsed.outputs.evidence,
            PathBuf::from("outputs/evidence.json")
        );
        assert_eq!(
            parsed.outputs.finalization,
            PathBuf::from("outputs/finalization.json")
        );
        assert_eq!(
            parsed.outputs.input_inventory,
            PathBuf::from("outputs/input_inventory.json")
        );
        assert_eq!(
            parsed.outputs.qualified_product,
            Some(PathBuf::from("outputs/qualified/product"))
        );
        assert_eq!(
            parsed.wrapper,
            [
                "./wrapper",
                "--product",
                "a path with spaces",
                "",
                "$(literal)",
                "--"
            ]
        );
        let mut rlib = host();
        rlib.extend(["--", "./wrapper"].map(OsString::from));
        assert!(arguments(rlib).unwrap().outputs.qualified_product.is_none());
    }

    #[test]
    fn missing_duplicate_unknown_and_positional_host_inputs_refuse() {
        let valid = host();
        for index in (0..valid.len()).step_by(2) {
            let mut missing = valid.clone();
            missing.drain(index..index + 2);
            missing.extend(["--", "./wrapper"].map(OsString::from));
            assert!(arguments(missing).is_err(), "missing option {index}");
            let mut duplicate = valid.clone();
            duplicate.extend([valid[index].clone(), valid[index + 1].clone()]);
            duplicate.extend(["--", "./wrapper"].map(OsString::from));
            assert!(arguments(duplicate).is_err(), "duplicate option {index}");
        }
        for unexpected in ["--unknown", "ambient.json", "--product=other"] {
            let mut input = valid.clone();
            input.extend([unexpected, "--", "./wrapper"].map(OsString::from));
            assert!(arguments(input).is_err());
        }
        for tail in [vec![], vec!["--"], vec!["--", ""]] {
            let mut input = valid.clone();
            input.extend(tail.into_iter().map(OsString::from));
            assert!(arguments(input).is_err());
        }
    }

    #[test]
    fn malformed_values_and_non_utf8_wrapper_refuse_without_lossy_conversion() {
        for value in [
            OsString::new(),
            OsString::from("--product"),
            OsString::from_vec(vec![b'a', 0]),
        ] {
            let mut input = host();
            input[1] = value;
            input.extend(["--", "./wrapper"].map(OsString::from));
            assert!(arguments(input).is_err());
        }
        let mut input = host();
        input.extend([
            OsString::from("--"),
            OsString::from("./wrapper"),
            OsString::from_vec(vec![0xff]),
        ]);
        assert!(arguments(input).is_err());
        let mut input = host();
        input.extend([
            OsString::from("--"),
            OsString::from("./wrapper"),
            OsString::from_vec(vec![b'x', 0]),
        ]);
        assert!(arguments(input).is_err());
    }
}
