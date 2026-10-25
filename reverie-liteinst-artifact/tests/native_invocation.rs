// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Synthetic records from the frozen normal-wrapper source contract. These
//! controls establish parsing behavior, not an observed compiler invocation,
//! successful action, complete declared inputs, or native qualification.

use reverie_liteinst_artifact::native_invocation::MAX_ARGUMENTS;
use reverie_liteinst_artifact::native_invocation::MAX_ENV_ENTRIES;
use reverie_liteinst_artifact::native_invocation::MAX_INVOCATION_BYTES;
use reverie_liteinst_artifact::native_invocation::MAX_RESPONSE_BYTES;
use reverie_liteinst_artifact::native_invocation::MAX_VALUE_BYTES;
use reverie_liteinst_artifact::native_invocation::parse_invocation_handoff;
use serde_json::Value;
use serde_json::json;

fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let word = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[((word >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((word >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((word >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn record(
    argv: &[&[u8]],
    env: &[(&[u8], &[u8])],
    cwd: &[u8],
    path: &[u8],
    response: &[u8],
) -> Value {
    json!({
        "version": 1,
        "argv_base64": argv.iter().map(|arg| encode(arg)).collect::<Vec<_>>(),
        "env_base64": env.iter().map(|(name, value)| [encode(name), encode(value)]).collect::<Vec<_>>(),
        "cwd_base64": encode(cwd),
        "response_file": {
            "path_base64": encode(path),
            "contents_base64": encode(response)
        }
    })
}

fn fixture() -> Value {
    record(
        &[
            b"/toolchain/rustc",
            b"--sysroot=/toolchain/sys root",
            b"@/private/missing-at-parse/rustc.args",
        ],
        &[(b"PATH", b"/toolchain/bin"), (b"GENERATED", b"value\nnext")],
        b"/project/work",
        b"/private/missing-at-parse/rustc.args",
        b"--crate-type=cdylib\n--emit=link=outputs/libplugin.so\n--extern=helper=inputs/libhelper.rlib\n--remap-path-prefix=/build/root=package/\nsrc/root.rs\n",
    )
}

fn parse(
    value: &Value,
) -> reverie_liteinst_artifact::Result<
    reverie_liteinst_artifact::native_invocation::InvocationObservation,
> {
    parse_invocation_handoff(&serde_json::to_vec(value).unwrap())
}

#[test]
fn normalized_handoff_retains_exact_compiler_arguments_environment_and_response() {
    let observed = parse(&fixture()).unwrap();
    let arguments: Vec<Vec<u8>> = [
        b"--crate-type=cdylib".as_slice(),
        b"--emit=link=outputs/libplugin.so",
        b"--extern=helper=inputs/libhelper.rlib",
        b"--remap-path-prefix=/build/root=package/",
        b"src/root.rs",
    ]
    .into_iter()
    .map(<[u8]>::to_vec)
    .collect();
    assert_eq!(observed.response_arguments, arguments);
    let mut expanded = vec![
        b"/toolchain/rustc".to_vec(),
        b"--sysroot=/toolchain/sys root".to_vec(),
    ];
    expanded.extend(arguments);
    assert_eq!(observed.expanded_argv, expanded);
    assert_eq!(observed.argv.len(), 3);
    assert_eq!(observed.cwd, b"/project/work");
    assert_eq!(
        observed.env[0],
        (b"PATH".to_vec(), b"/toolchain/bin".to_vec())
    );
    assert_eq!(
        observed.env[1],
        (b"GENERATED".to_vec(), b"value\nnext".to_vec())
    );
    assert_eq!(
        observed.response_file.path,
        b"/private/missing-at-parse/rustc.args"
    );
    assert_eq!(
        observed.response_file.contents,
        b"--crate-type=cdylib\n--emit=link=outputs/libplugin.so\n--extern=helper=inputs/libhelper.rlib\n--remap-path-prefix=/build/root=package/\nsrc/root.rs\n"
    );
}

#[test]
fn arbitrary_posix_bytes_survive_until_explicit_compiler_flag_validation() {
    let observed = parse(&record(
        &[b"/tools/\xffrustc", b"@/scratch/\xfe.args"],
        &[(b"NAME\xfd", b"value\xfc")],
        b"/work/\xfb",
        b"/scratch/\xfe.args",
        b"--crate-type=rlib\n--emit=link=out/lib\xfa.rlib\nsrc/\xf9.rs\n",
    ))
    .unwrap();
    assert_eq!(observed.argv[0], b"/tools/\xffrustc");
    assert_eq!(
        observed.env,
        vec![(b"NAME\xfd".to_vec(), b"value\xfc".to_vec())]
    );
    assert_eq!(observed.cwd, b"/work/\xfb");
    assert_eq!(observed.response_file.path, b"/scratch/\xfe.args");
    assert_eq!(observed.response_arguments[2], b"src/\xf9.rs");
}

#[test]
fn response_lines_are_arguments_without_shell_splitting_or_second_normalization() {
    let mut value = fixture();
    let response =
        b"--cfg=feature=\"two words\"\n\n path with spaces \n\ttab\t\nback\\slash\n$(literal)\n";
    value["response_file"]["contents_base64"] = json!(encode(response));
    let observed = parse(&value).unwrap();
    assert_eq!(
        observed.response_arguments,
        [
            b"--cfg=feature=\"two words\"".as_slice(),
            b"",
            b" path with spaces ",
            b"\ttab\t",
            b"back\\slash",
            b"$(literal)",
        ]
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>()
    );
    assert_eq!(observed.response_file.contents, response);
    // Effective environment bytes have already been normalized by the wrapper.
    // A literal backslash-n must remain distinct from an actual newline.
    let mut escaped = fixture();
    escaped["env_base64"][1][1] = json!(encode(b"value\\nnext"));
    assert_ne!(parse(&escaped).unwrap().env, parse(&fixture()).unwrap().env);
}

#[test]
fn nested_response_references_remain_unresolved_data() {
    let mut value = fixture();
    value["response_file"]["contents_base64"] = json!(encode(
        b"--crate-type=rlib\n@declared/nested.args\n@another.args\n"
    ));
    let observed = parse(&value).unwrap();
    assert_eq!(observed.response_arguments[1], b"@declared/nested.args");
    assert_eq!(observed.response_arguments[2], b"@another.args");
    assert_eq!(observed.expanded_argv[3], b"@declared/nested.args");
    assert_eq!(observed.expanded_argv[4], b"@another.args");
}

#[test]
fn response_reference_requires_full_path_exactly_once_at_the_end() {
    for replacement in [
        b"@/other/rustc.args".as_slice(),
        b"@rustc.args",
        b"/private/missing-at-parse/rustc.args",
    ] {
        let mut value = fixture();
        value["argv_base64"][2] = json!(encode(replacement));
        assert!(
            parse(&value).is_err(),
            "different response reference accepted"
        );
    }
    let mut twice = fixture();
    twice["argv_base64"][1] = twice["argv_base64"][2].clone();
    assert!(parse(&twice).is_err());
    let mut tail = fixture();
    tail["argv_base64"]
        .as_array_mut()
        .unwrap()
        .push(json!(encode(b"--tail")));
    assert!(parse(&tail).is_err());
    let mut no_executable = fixture();
    no_executable["argv_base64"] = json!([encode(b"@/private/missing-at-parse/rustc.args")]);
    assert!(parse(&no_executable).is_err());
    let mut empty_executable = fixture();
    empty_executable["argv_base64"][0] = json!("");
    assert!(parse(&empty_executable).is_err());
}

#[test]
fn malformed_response_lines_refuse_instead_of_silently_rewriting() {
    for response in [
        b"".as_slice(),
        b"argument without terminal LF",
        b"one\r\ntwo\n",
        b"one\rtwo\n",
        b"one\0two\n",
    ] {
        let mut value = fixture();
        value["response_file"]["contents_base64"] = json!(encode(response));
        assert!(parse(&value).is_err());
    }
    let mut empty_argument = fixture();
    empty_argument["response_file"]["contents_base64"] = json!(encode(b"\n"));
    assert_eq!(
        parse(&empty_argument).unwrap().response_arguments,
        vec![Vec::<u8>::new()]
    );
}

#[test]
fn environment_order_is_retained_and_duplicate_or_invalid_names_refuse() {
    let original = parse(&fixture()).unwrap();
    let mut swapped = fixture();
    swapped["env_base64"].as_array_mut().unwrap().swap(0, 1);
    let observed = parse(&swapped).unwrap();
    assert_eq!(observed.env[0], original.env[1]);
    assert_ne!(observed.env, original.env);
    for name in [b"PATH".as_slice(), b"", b"BAD=NAME", b"BAD\0NAME"] {
        let mut value = fixture();
        value["env_base64"][1][0] = json!(encode(name));
        assert!(parse(&value).is_err());
    }
    let mut empty = fixture();
    empty["env_base64"][0][1] = json!("");
    assert!(parse(&empty).unwrap().env[0].1.is_empty());
}

#[test]
fn nul_refuses_in_all_process_strings_and_relative_cwd_refuses() {
    for location in [
        "/argv_base64/0",
        "/argv_base64/1",
        "/env_base64/0/0",
        "/env_base64/0/1",
        "/cwd_base64",
        "/response_file/path_base64",
    ] {
        let mut value = fixture();
        *value.pointer_mut(location).unwrap() = json!(encode(b"/before\0after"));
        assert!(parse(&value).is_err(), "NUL accepted at {location}");
    }
    for cwd in [b"".as_slice(), b"relative/work"] {
        let mut value = fixture();
        value["cwd_base64"] = json!(encode(cwd));
        assert!(parse(&value).is_err());
    }
}

#[test]
fn standard_base64_known_vectors_preserve_all_octets_and_padding_lengths() {
    for (encoded, expected) in [
        ("", b"".as_slice()),
        ("Zg==", b"f"),
        ("Zm8=", b"fo"),
        ("Zm9v", b"foo"),
        ("+/8=", &[251, 255]),
    ] {
        let mut value = fixture();
        value["env_base64"][0][1] = json!(encoded);
        assert_eq!(parse(&value).unwrap().env[0].1, expected);
    }
    // NUL is intentionally excluded: POSIX process strings cannot contain it.
    let octets: Vec<u8> = (1..=255).collect();
    let mut value = fixture();
    value["env_base64"][0][1] = json!(encode(&octets));
    assert_eq!(parse(&value).unwrap().env[0].1, octets);
}

#[test]
fn malformed_or_noncanonical_base64_refuses() {
    for encoded in [
        "Zg", "Zg=", "Zg===", "Zh==", "Zm9=", "Zg==Zg==", "=m9v", "Zm=v", "Zm9v\n", " Zg==",
        "-_8=", "é===", "====",
    ] {
        let mut value = fixture();
        value["env_base64"][0][1] = json!(encoded);
        assert!(
            parse(&value).is_err(),
            "noncanonical encoding accepted: {encoded:?}"
        );
    }
}

#[test]
fn exact_wire_shape_refuses_defaults_unknown_fields_and_duplicate_keys() {
    for version in [json!(0), json!(2), json!(null), json!("1"), json!(true)] {
        let mut value = fixture();
        value["version"] = version;
        assert!(parse(&value).is_err());
    }
    for field in [
        "version",
        "argv_base64",
        "env_base64",
        "cwd_base64",
        "response_file",
    ] {
        let mut value = fixture();
        value.as_object_mut().unwrap().remove(field);
        assert!(parse(&value).is_err(), "missing {field} accepted");
    }
    let mut success = fixture();
    success["success"] = json!(true);
    assert!(parse(&success).is_err());
    let mut extra_response = fixture();
    extra_response["response_file"]["sha256"] = json!("caller-asserted");
    assert!(parse(&extra_response).is_err());
    let mut malformed_pair = fixture();
    malformed_pair["env_base64"][0] = json!([encode(b"PATH"), encode(b"value"), encode(b"extra")]);
    assert!(parse(&malformed_pair).is_err());
    let bytes = serde_json::to_string(&fixture()).unwrap();
    let duplicate = bytes.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert_ne!(duplicate, bytes);
    assert!(parse_invocation_handoff(duplicate.as_bytes()).is_err());
    let duplicate_response = bytes.replacen(
        "\"response_file\":{",
        "\"response_file\":{\"path_base64\":\"YQ==\",",
        1,
    );
    assert_ne!(duplicate_response, bytes);
    assert!(parse_invocation_handoff(duplicate_response.as_bytes()).is_err());
    assert!(parse_invocation_handoff(format!("{bytes} {{}}").as_bytes()).is_err());
}

#[test]
fn record_argument_environment_and_value_bounds_refuse_without_truncation() {
    assert!(parse_invocation_handoff(&vec![b' '; MAX_INVOCATION_BYTES + 1]).is_err());
    let mut many_arguments = fixture();
    many_arguments["argv_base64"] = json!(vec![encode(b"x"); MAX_ARGUMENTS + 1]);
    assert!(parse(&many_arguments).is_err());
    let mut many_env = fixture();
    many_env["env_base64"] = json!(
        (0..=MAX_ENV_ENTRIES)
            .map(|index| [encode(index.to_string().as_bytes()), encode(b"value")])
            .collect::<Vec<_>>()
    );
    assert!(parse(&many_env).is_err());
    let mut oversized_value = fixture();
    oversized_value["env_base64"][0][1] = json!(encode(&vec![b'x'; MAX_VALUE_BYTES + 1]));
    assert!(parse(&oversized_value).is_err());
    let mut many_lines = fixture();
    many_lines["response_file"]["contents_base64"] = json!(encode(&vec![b'\n'; MAX_ARGUMENTS]));
    // The retained response plus the actual command prefix exceed the argv bound.
    assert!(parse(&many_lines).is_err());
}

#[test]
fn response_byte_bound_is_independent_of_the_encoded_record_bound() {
    let mut oversized = fixture();
    let mut response = vec![b'x'; MAX_RESPONSE_BYTES];
    response.push(b'\n');
    oversized["response_file"]["contents_base64"] = json!(encode(&response));
    let bytes = serde_json::to_vec(&oversized).unwrap();
    assert!(bytes.len() < MAX_INVOCATION_BYTES);
    assert!(parse_invocation_handoff(&bytes).is_err());
}

#[test]
fn output_paths_and_argument_boundaries_are_not_folded_into_equivalence() {
    let original = parse(&fixture()).unwrap();
    let mut different_output = fixture();
    different_output["response_file"]["contents_base64"] = json!(encode(
        b"--crate-type=cdylib\n--emit=link=other/libplugin.so\nsrc/root.rs\n"
    ));
    let changed = parse(&different_output).unwrap();
    assert_ne!(
        original.response_arguments[1],
        changed.response_arguments[1]
    );
    let mut combined = fixture();
    combined["response_file"]["contents_base64"] = json!(encode(b"--cfg feature\n"));
    let mut separate = fixture();
    separate["response_file"]["contents_base64"] = json!(encode(b"--cfg\nfeature\n"));
    assert_ne!(
        parse(&combined).unwrap().expanded_argv,
        parse(&separate).unwrap().expanded_argv
    );
}
