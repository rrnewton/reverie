// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Finite synthetic adapter controls for owned subprocess/custody mechanics.
//! The Python fixture does not invoke rustc. Its tiny rlib stand-ins are not
//! native compiler evidence or ELF qualification positives. Expectations come
//! from fixture source/recipe bytes before execution, never from its handoff or
//! the factory's retained evidence. No Buck executable is used.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie_liteinst_artifact::Result;
use reverie_liteinst_artifact::Runner;
use reverie_liteinst_artifact::native_action::Argument;
use reverie_liteinst_artifact::native_action::ArgumentPart;
use reverie_liteinst_artifact::native_action::ContextSlot;
use reverie_liteinst_artifact::native_action::DeclaredGraph;
use reverie_liteinst_artifact::native_action::DeclaredProducer;
use reverie_liteinst_artifact::native_action::EnvironmentTool;
use reverie_liteinst_artifact::native_action::ExpectedFile;
use reverie_liteinst_artifact::native_action::ExpectedGraph;
use reverie_liteinst_artifact::native_action::ExpectedProducer;
use reverie_liteinst_artifact::native_action::OwnedProduct;
use reverie_liteinst_artifact::native_action::OwnerPin;
use reverie_liteinst_artifact::native_action::ProducedArtifact;
use reverie_liteinst_artifact::native_action::ProducerOutput;
use reverie_liteinst_artifact::native_action::Profile;
use reverie_liteinst_artifact::native_action::PythonWrapperEnvironment;
use reverie_liteinst_artifact::native_action::WrapperRequest;
use reverie_liteinst_artifact::native_action::with_declared_private_static;
use reverie_liteinst_artifact::native_action::with_owned_rust_action;
use reverie_liteinst_artifact::sha256;
use serde_json::Value;
use serde_json::json;

static NEXT: AtomicU64 = AtomicU64::new(0);

const WRAPPER: &str = r#"import argparse, base64, json, os, pathlib, sys
p = argparse.ArgumentParser()
p.add_argument('--invocation-handoff', required=True)
p.add_argument('--product', required=True)
p.add_argument('--response', required=True)
p.add_argument('--config', required=True)
a = p.parse_args()
c = json.loads(pathlib.Path(a.config).read_bytes())
if 'expected_wrapper_environment' in c:
    assert dict(os.environ) == c['expected_wrapper_environment'], 'actual wrapper child environment differs'
mode = c['mode']
response = pathlib.Path(a.response).read_bytes()
compiler = c['compiler']
cwd = os.getcwd()
environment = [['FIXTURE_MODE', 'synthetic-adapter-only']]
for key in c.get('context_keys', []):
    if key in os.environ: environment.append([key, os.environ[key]])
if mode == 'wrong-compiler-context':
    for pair in environment:
        if pair[0] == 'EXECUTION_ID': pair[1] += '.wrong'
if mode == 'extra-compiler-context': environment.append(['UNEXPECTED_COMPILER_CONTEXT', 'extra'])
if mode == 'wrong-compiler': compiler += '.other'
if mode == 'wrong-profile': response = response.replace(b'-Copt-level=3', b'-Copt-level=2')
if mode == 'wrong-features': response += b'--cfg=feature="unexpected"\n'
if mode == 'wrong-source': response = response.replace(os.fsencode(c['source']), os.fsencode(c['source'] + '.other'))
if mode == 'wrong-product': response = response.replace(os.fsencode(a.product), os.fsencode(a.product + '.other'))
if mode == 'wrong-response': response = response[:-1]
if mode == 'wrong-environment': environment[0][1] = 'different'
if mode == 'wrong-cwd': cwd += '/other'
if mode in ('mutate-source', 'fail-and-mutate', 'recipe-and-mutate'):
    with open(c['source'], 'ab') as source: source.write(b'\nchanged during owned command\n')
if mode == 'mutate-metadata':
    with open(c['executor_metadata'], 'ab') as metadata: metadata.write(b' ')
if mode == 'mutate-expected':
    with open(c['expected_manifest'], 'ab') as manifest: manifest.write(b' ')
if mode == 'add-source-member':
    with open(pathlib.Path(c['source']).parent / 'unlisted.rs', 'xb') as source:
        source.write(b'pub fn unlisted_during_command() {}\n')
if mode == 'recipe-and-mutate': response = response.replace(b'-Copt-level=3', b'-Copt-level=2')
def b64(value): return base64.b64encode(value).decode('ascii')
temporary = str(pathlib.Path(a.invocation_handoff).parent / 'removed-response.args')
handoff = {
    'version': 1,
    'argv_base64': [b64(os.fsencode(compiler)), b64(os.fsencode('@' + temporary))],
    'env_base64': [[b64(os.fsencode(k)), b64(os.fsencode(v))] for k,v in environment],
    'cwd_base64': b64(os.fsencode(cwd)),
    'response_file': {'path_base64': b64(os.fsencode(temporary)), 'contents_base64': b64(response)},
}
with open(a.invocation_handoff, 'xb') as output:
    output.write(json.dumps(handoff, separators=(',', ':')).encode() + b'\n')
product = c['product_prefix'].encode()
if c['dependency'] is not None: product += pathlib.Path(c['dependency']).read_bytes()
if mode != 'missing-product':
    with open(a.product, 'xb') as output: output.write(product)
print('synthetic adapter result; no Rust compiler executed')
if mode in ('wrapper-failure', 'fail-and-mutate'):
    print('synthetic final wrapper failure after handoff', file=sys.stderr)
    sys.exit(17)
"#;

fn literal(bytes: impl AsRef<[u8]>) -> Argument {
    vec![ArgumentPart::Literal {
        bytes: bytes.as_ref().to_vec(),
    }]
}

fn artifact(id: &str) -> Argument {
    vec![ArgumentPart::Artifact { id: id.into() }]
}

fn composed(prefix: &[u8], id: &str, suffix: &[u8]) -> Argument {
    vec![
        ArgumentPart::Literal {
            bytes: prefix.to_vec(),
        },
        ArgumentPart::Artifact { id: id.into() },
        ArgumentPart::Literal {
            bytes: suffix.to_vec(),
        },
    ]
}

fn render(argument: &Argument, paths: &BTreeMap<String, PathBuf>) -> Vec<u8> {
    argument
        .iter()
        .flat_map(|part| match part {
            ArgumentPart::Literal { bytes } => bytes.clone(),
            ArgumentPart::Artifact { id } => paths[id].as_os_str().as_bytes().to_vec(),
            ArgumentPart::ExecutionContext { .. } => {
                panic!("synthetic argv fixture has no context substitutions")
            }
        })
        .collect()
}

// Independently specify Runner's documented subprocess environment before the
// transaction. No process-global environment mutation or receipt readback.
fn wrapper_environment() -> BTreeMap<String, Argument> {
    let mut result: BTreeMap<_, _> = std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.into_string().expect("fixture environment key is UTF-8");
            if [
                "LD_PRELOAD",
                "HERMIT_LITEINST_STAGE",
                "HERMIT_LITEINST_TOOL_RUNTIME",
                "REVERIE_LITEINST_PRELOAD",
                "REVERIE_LITEINST_TEST_RUNTIME_MANIFEST",
            ]
            .contains(&key.as_str())
            {
                None
            } else {
                Some((key, literal(value.as_os_str().as_bytes())))
            }
        })
        .collect();
    result.insert("LC_ALL".into(), literal(b"C"));
    result
}

struct Action {
    home: PathBuf,
    expected: ExpectedGraph,
    declared: DeclaredGraph,
    request: WrapperRequest,
    product_bytes: Vec<u8>,
}

struct Fixture {
    root: PathBuf,
    runner: Runner,
}

// Author expected control bytes independently. Explicit calls below keep
// intentionally changed expectations aligned without deriving any expectation
// from the observed wrapper handoff, retained record, or executor metadata.
fn seal_expected(action: &Action) {
    fs::write(
        &action.declared.leaf.control_inputs["expected"],
        serde_json::to_vec(&action.expected).unwrap(),
    )
    .unwrap();
}

// The synthetic executor inventories all current physical leaves, including
// forwarded producer/control outputs. It does not author independent hashes.
// Call explicitly after producer outputs or independently authored inputs change;
// never refresh automatically inside run or after an intentional row mutation.
fn refresh_inventory(action: &Action) {
    fn collect(path: &std::path::Path, rows: &mut BTreeMap<PathBuf, u64>) {
        let metadata = fs::metadata(path).unwrap();
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                collect(&entry.unwrap().path(), rows);
            }
        } else {
            assert!(metadata.is_file());
            rows.insert(fs::canonicalize(path).unwrap(), metadata.len());
        }
    }
    let mut rows = BTreeMap::new();
    for path in action
        .declared
        .leaf
        .files
        .values()
        .chain(action.declared.leaf.control_inputs.values())
        .chain(std::iter::once(&action.declared.leaf.recipe))
    {
        collect(path, &mut rows);
    }
    assert!(!rows.contains_key(&action.request.executor_metadata));
    let digests: Vec<_> = rows
        .into_iter()
        .map(|(path, size)| {
            json!({"path":path.strip_prefix(&action.request.cwd).unwrap(),
            "digest":format!("{}:{size}", "a".repeat(40))})
        })
        .collect();
    fs::write(
        &action.request.executor_metadata,
        serde_json::to_vec(&json!({"version":1,"digests":digests})).unwrap(),
    )
    .unwrap();
}

fn expected_input(path: &std::path::Path) -> ExpectedFile {
    if path.is_dir() {
        fn members(
            root: &std::path::Path,
            at: &std::path::Path,
            out: &mut BTreeMap<PathBuf, ExpectedFile>,
        ) {
            for entry in fs::read_dir(at).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    members(root, &path, out);
                } else {
                    out.insert(
                        path.strip_prefix(root).unwrap().into(),
                        expected_input(&path),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        members(path, path, &mut files);
        ExpectedFile {
            produced: None,
            directory_members: Some(files),
            sha256: String::new(),
            size: 0,
            source_key: None,
            compiler_filename: None,
        }
    } else {
        let bytes = fs::read(path).unwrap();
        ExpectedFile {
            produced: None,
            directory_members: None,
            sha256: sha256(&bytes),
            size: bytes.len() as u64,
            source_key: None,
            compiler_filename: None,
        }
    }
}

fn role_value(producer: &DeclaredProducer, all: &BTreeMap<String, DeclaredProducer>) -> Value {
    let mut artifacts = BTreeMap::from([
        ("product", producer.product.clone()),
        ("recipe", producer.recipe.clone()),
        ("directory", producer.directory.clone()),
        ("invocation", producer.invocation.clone()),
        ("evidence", producer.evidence.clone()),
        ("finalization", producer.finalization.clone()),
        ("input_inventory", producer.input_inventory.clone()),
    ]);
    if let Some(path) = &producer.qualified_product {
        artifacts.insert("qualified_product", path.clone());
    }
    if let Some(path) = &producer.metadata_full {
        artifacts.insert("metadata_full", path.clone());
    }
    let roles: BTreeMap<_, _> = producer
        .dependencies
        .values()
        .chain(producer.metadata_dependencies.values())
        .map(|role| (role.clone(), role_value(&all[role], all)))
        .collect();
    json!({"configured_target":producer.producer,"configuration":producer.configuration,
        "selection":producer.selection,"artifacts":artifacts,"declared_inputs":producer.declared_inputs,
        "input_roots":producer.input_roots,"consumed_inputs":producer.consumed_inputs,
        "linked_inputs":producer.linked_inputs,
        "control_inputs":producer.control_inputs,"roles":roles})
}

fn seal_fixture_recipe(
    expected: &mut ExpectedProducer,
    leaf: &mut DeclaredProducer,
    all: &BTreeMap<String, DeclaredProducer>,
    expected_all: &BTreeMap<String, ExpectedProducer>,
) {
    // The synthetic fixture authors the normal recipe independently of handoff
    // records. Companion expectations name typed roles before outputs exist.
    let mut roles = BTreeMap::new();
    for (name, producer) in all {
        let role = role_value(producer, all);
        for kind in [
            "artifacts",
            "declared_inputs",
            "input_roots",
            "consumed_inputs",
            "linked_inputs",
            "control_inputs",
        ] {
            let namespace = match kind {
                "artifacts" => "artifact",
                "declared_inputs" => "declared",
                "input_roots" => "root",
                "consumed_inputs" => "consumed",
                "linked_inputs" => "linked",
                "control_inputs" => "control",
                _ => unreachable!(),
            };
            for (id, path) in role[kind].as_object().unwrap() {
                leaf.declared_inputs.insert(
                    format!("role/{name}/{namespace}/{id}"),
                    PathBuf::from(path.as_str().unwrap()),
                );
            }
        }
        roles.insert(name.clone(), role);
    }
    leaf.files = leaf.declared_inputs.clone();
    leaf.files.extend(leaf.input_roots.clone());
    leaf.files.extend(leaf.consumed_inputs.clone());
    leaf.files.extend(leaf.linked_inputs.clone());
    let pinned = expected.files.clone();
    expected.files = leaf
        .files
        .iter()
        .map(|(id, path)| {
            let produced = |role: &str, output: ProducerOutput| ExpectedFile {
                produced: Some(ProducedArtifact {
                    role: role.into(),
                    output,
                }),
                directory_members: None,
                sha256: String::new(),
                size: 0,
                source_key: None,
                compiler_filename: None,
            };
            let mut spec = None;
            for role in all.keys() {
                if let Some(kind) = id.strip_prefix(&format!("role/{role}/artifact/")) {
                    let output = match kind {
                        "product" => ProducerOutput::Product,
                        "metadata_full" => ProducerOutput::MetadataFull,
                        "recipe" => ProducerOutput::Recipe,
                        "invocation" => ProducerOutput::Invocation,
                        "evidence" => ProducerOutput::Evidence,
                        "finalization" => ProducerOutput::Finalization,
                        "directory" => ProducerOutput::Directory,
                        "input_inventory" => ProducerOutput::InputInventory,
                        _ => panic!("unexpected fixture output"),
                    };
                    spec = Some(produced(role, output));
                }
                for namespace in ["declared", "root", "consumed"] {
                    if let Some(input) = id.strip_prefix(&format!("role/{role}/{namespace}/")) {
                        spec = Some(expected_all[role].files[input].clone());
                    }
                }
            }
            for role in all.keys() {
                if let Some(control) = id.strip_prefix(&format!("role/{role}/control/")) {
                    spec = Some(produced(
                        role,
                        match control {
                            "expected" => ProducerOutput::ExpectedInputs,
                            "owner" => ProducerOutput::OwnerExecutable,
                            _ => panic!("unexpected fixture control"),
                        },
                    ));
                }
            }
            if matches!(id.as_str(), "dependency-code" | "declared:dependency-code") {
                spec = Some(produced(
                    leaf.dependencies.values().next().unwrap(),
                    ProducerOutput::Product,
                ));
            }
            let spec = spec.unwrap_or_else(|| {
                let input = id.strip_prefix("declared:").unwrap_or(id);
                pinned.get(input).cloned().unwrap_or_else(|| {
                    assert_eq!(input, "source-tree");
                    expected_input(path)
                })
            });
            (id.clone(), spec)
        })
        .collect();
    let mut output_roles = vec![
        "product",
        "directory",
        "invocation",
        "evidence",
        "finalization",
        "input_inventory",
    ];
    if leaf.selection == "cdylib" {
        output_roles.push("qualified_product");
    }
    let recipe = json!({"version":3,"configured_target":leaf.producer,"configuration":leaf.configuration,
        "selection":leaf.selection,"crate_type":match leaf.selection.as_str() {"cdylib" => "cdylib", "staticlib" => "staticlib", _ => "rlib"},
        "emit":if matches!(leaf.selection.as_str(), "cdylib" | "staticlib") {"link"} else {"rlib"},"dependency_link_strategy":"static_pic",
        "declared_features":expected.features,"source_tree":leaf.source_tree,
        "product_short_path":leaf.product.file_name().unwrap().to_str().unwrap(),"output_roles":output_roles,
        "declared_inputs":leaf.declared_inputs,"input_roots":leaf.input_roots,
        "consumed_inputs":leaf.consumed_inputs,"linked_inputs":leaf.linked_inputs,
        "control_inputs":leaf.control_inputs,"roles":roles});
    fs::write(&leaf.recipe, serde_json::to_vec(&recipe).unwrap()).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "native-action-controls-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let runner = Runner::new(root.join("logs"), Duration::from_secs(20), 65536).unwrap();
        Self { root, runner }
    }

    fn action(
        &self,
        role: &str,
        selection: &str,
        mode: &str,
        dependency: Option<&Action>,
    ) -> Action {
        let home = self.root.join(role);
        fs::create_dir(&home).unwrap();
        fs::create_dir(home.join("owners")).unwrap();
        fs::create_dir(home.join("sources")).unwrap();
        let directory = home.join("owners/action");
        let product = home.join(match selection {
            "cdylib" => "product.so",
            "staticlib" => "product.a",
            _ => "product.rlib",
        });
        let python = fs::canonicalize("/usr/bin/python3").expect("fixture requires Python 3");
        let mut paths = BTreeMap::from([
            ("python".into(), python.clone()),
            ("wrapper".into(), home.join("wrapper.py")),
            ("compiler".into(), home.join("compiler-stand-in")),
            ("source".into(), home.join("sources/source.rs")),
            ("response".into(), home.join("response-input.args")),
            ("config".into(), home.join("fixture-config.json")),
        ]);
        let mut contents: BTreeMap<String, Vec<u8>> = BTreeMap::from([
            ("python".into(), fs::read(&python).unwrap()),
            ("wrapper".into(), WRAPPER.as_bytes().to_vec()),
            (
                "compiler".into(),
                b"synthetic compiler identity; never executed\n".to_vec(),
            ),
            ("source".into(), b"pub fn fixture_source() {}\n".to_vec()),
        ]);
        let mut dependencies = BTreeMap::new();
        let mut expected_dependencies = BTreeMap::new();
        let mut declared_dependencies = BTreeMap::new();
        if let Some(dep) = dependency {
            paths.insert("dependency-code".into(), dep.declared.leaf.product.clone());
            contents.insert("dependency-code".into(), dep.product_bytes.clone());
            dependencies.insert(
                "declared:dependency-code".into(),
                dep.expected.leaf.role.clone(),
            );
            expected_dependencies.insert(dep.expected.leaf.role.clone(), dep.expected.leaf.clone());
            let mut edge = dep.declared.leaf.clone();
            // Stable-toolchain metadata may alias its exact code product.
            edge.metadata_full = Some(edge.product.clone());
            declared_dependencies.insert(edge.role.clone(), edge);
        }
        let mut recipe_paths = paths.clone();
        recipe_paths.insert("$product".into(), product.clone());
        recipe_paths.insert("$directory".into(), directory.clone());
        let mut compiler_argv = vec![
            artifact("compiler"),
            literal(b"--crate-name"),
            literal(role.replace('-', "_")),
            literal(b"--crate-type"),
            literal(match selection {
                "cdylib" => "cdylib",
                "staticlib" => "staticlib",
                _ => "rlib",
            }),
            composed(b"--emit=link=", "$product", b""),
            literal(b"-Copt-level=3"),
            literal(b"-Cdebuginfo=0"),
            literal(b"-Cdebug-assertions=yes"),
            literal(b"-Coverflow-checks=yes"),
            literal(b"-Crelocation-model=pic"),
            literal(b"--cfg=feature=\"fixture_feature\""),
            artifact("source"),
        ];
        if dependency.is_some() {
            compiler_argv.push(composed(b"--extern=support=", "dependency-code", b""));
        }
        let mut response = Vec::new();
        for arg in &compiler_argv[1..] {
            response.extend(render(arg, &recipe_paths));
            response.push(b'\n');
        }
        contents.insert("response".into(), response);
        let product_prefix = format!("synthetic rlib stand-in for {role}; not Rust output\n");
        let mut product_bytes = product_prefix.as_bytes().to_vec();
        if let Some(dep) = dependency {
            product_bytes.extend_from_slice(&dep.product_bytes);
        }
        let config = json!({
            "mode": mode,
            "expected_manifest": home.join("expected.json"),
            "executor_metadata": home.join("executor-inputs.json"),
            "compiler": paths["compiler"],
            "source": paths["source"],
            "product_prefix": product_prefix,
            "dependency": dependency.map(|dep| dep.declared.leaf.product.clone())
        });
        contents.insert("config".into(), serde_json::to_vec(&config).unwrap());
        for (id, bytes) in &contents {
            if id != "python" && id != "dependency-code" {
                fs::write(&paths[id], bytes).unwrap();
            }
        }
        fs::set_permissions(&paths["compiler"], fs::Permissions::from_mode(0o755)).unwrap();
        let files = contents
            .iter()
            .map(|(id, bytes)| {
                (
                    id.clone(),
                    ExpectedFile {
                        produced: None,
                        directory_members: None,
                        sha256: sha256(bytes),
                        size: bytes.len() as u64,
                        source_key: None,
                        compiler_filename: None,
                    },
                )
            })
            .collect();
        let wrapper_argv = vec![
            artifact("python"),
            literal(b"-B"),
            artifact("wrapper"),
            literal(b"--invocation-handoff"),
            composed(b"", "$directory", b"/invocation.json"),
            literal(b"--product"),
            artifact("$product"),
            literal(b"--response"),
            artifact("response"),
            literal(b"--config"),
            artifact("config"),
        ];
        let request = WrapperRequest {
            argv: wrapper_argv
                .iter()
                .map(|arg| String::from_utf8(render(arg, &recipe_paths)).unwrap())
                .collect(),
            cwd: PathBuf::from("/"),
            executor_metadata: home.join("executor-inputs.json"),
        };
        let mut expected = ExpectedProducer {
            owner_pin: None,
            role: role.into(),
            producer: format!("fixture//:{role}"),
            configuration: format!("fixture-platform ({role}-full)"),
            selection: selection.into(),
            crate_name: role.replace('-', "_"),
            features: vec!["fixture_feature".into()],
            profile: Profile {
                opt_level: "3".into(),
                debuginfo: 0,
                debug_assertions: true,
                overflow_checks: true,
                test: false,
            },
            files,
            wrapper_argv,
            wrapper_environment: wrapper_environment(),
            wrapper_environment_contract: None,
            external_linked_inputs: BTreeSet::new(),
            compiler_argv,
            compiler_environment: BTreeMap::from([(
                "FIXTURE_MODE".into(),
                literal(b"synthetic-adapter-only"),
            )]),
            cwd: literal(b"/"),
            dependencies: dependencies.clone(),
            metadata_dependencies: dependencies.clone(),
        };
        paths.insert("source-tree".into(), home.join("sources"));
        let mut declared = DeclaredProducer {
            role: role.into(),
            producer: expected.producer.clone(),
            configuration: expected.configuration.clone(),
            selection: selection.into(),
            files: paths.clone(),
            declared_inputs: paths.clone(),
            input_roots: paths
                .iter()
                .map(|(id, path)| (format!("declared:{id}"), path.clone()))
                .collect(),
            consumed_inputs: dependencies
                .keys()
                .map(|id| (id.clone(), paths["dependency-code"].clone()))
                .collect(),
            linked_inputs: if matches!(selection, "cdylib" | "staticlib") {
                dependencies
                    .keys()
                    .map(|id| (id.clone(), paths["dependency-code"].clone()))
                    .collect()
            } else {
                BTreeMap::new()
            },
            control_inputs: BTreeMap::from([
                ("expected".into(), home.join("expected.json")),
                (
                    "owner".into(),
                    fs::canonicalize(std::env::current_exe().unwrap()).unwrap(),
                ),
            ]),
            source_tree: home.join("sources"),
            recipe: home.join("action-inputs.json"),
            metadata_full: None,
            product,
            input_inventory: directory.join("input_inventory.json"),
            invocation: directory.join("invocation.json"),
            evidence: directory.join("evidence.json"),
            finalization: directory.join("finalization.json"),
            qualified_product: (selection == "cdylib").then(|| directory.join("qualified/product")),
            directory,
            metadata_dependencies: dependencies.clone(),
            dependencies,
        };
        seal_fixture_recipe(
            &mut expected,
            &mut declared,
            &declared_dependencies,
            &expected_dependencies,
        );
        let action = Action {
            home,
            expected: ExpectedGraph {
                leaf: expected,
                dependencies: expected_dependencies,
            },
            declared: DeclaredGraph {
                leaf: declared,
                dependencies: declared_dependencies,
            },
            request,
            product_bytes,
        };
        seal_expected(&action);
        if dependency.is_none_or(|dep| dep.declared.leaf.finalization.exists()) {
            refresh_inventory(&action);
        }
        action
    }

    fn run(&mut self, action: &Action) -> Result<OwnedProduct> {
        with_owned_rust_action(
            &action.expected,
            &action.declared,
            &action.request,
            &mut self.runner,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn refused(result: Result<OwnedProduct>) -> String {
    match result {
        Err(error) => error,
        Ok(OwnedProduct::Compilation(_)) => panic!("violating adapter returned Compilation"),
        Ok(OwnedProduct::Constructor(_)) => {
            panic!("synthetic adapter granted constructor qualification")
        }
        Ok(OwnedProduct::StandaloneConstructor(_)) => {
            panic!("synthetic adapter granted standalone qualification")
        }
        Ok(OwnedProduct::NativeEntry(_)) => {
            panic!("synthetic adapter granted explicit-entry qualification")
        }
    }
}

fn assert_unfinalized(action: &Action) {
    assert!(
        !action.declared.leaf.finalization.exists(),
        "failure published finalization"
    );
}

fn assert_compilation(result: Result<OwnedProduct>, action: &Action) {
    match result.unwrap_or_else(|error| panic!("synthetic owned rlib transaction failed: {error}"))
    {
        OwnedProduct::Compilation(compilation) => {
            assert!(!compilation.reused());
            assert_eq!(compilation.product().sha256, sha256(&action.product_bytes));
            assert_eq!(compilation.product().path, action.declared.leaf.product);
            assert_eq!(compilation.evidence().path, action.declared.leaf.evidence);
        }
        OwnedProduct::Constructor(_) => panic!("rlib stand-in became constructor authority"),
        OwnedProduct::StandaloneConstructor(_) => {
            panic!("rlib stand-in became standalone authority")
        }
        OwnedProduct::NativeEntry(_) => panic!("rlib stand-in became explicit-entry authority"),
    }
    assert_eq!(
        fs::read(&action.declared.leaf.product).unwrap(),
        action.product_bytes
    );
    let evidence = fs::read(&action.declared.leaf.evidence).unwrap();
    let finalization: Value =
        serde_json::from_slice(&fs::read(&action.declared.leaf.finalization).unwrap()).unwrap();
    assert_eq!(finalization["evidence_sha256"], sha256(&evidence));
    assert_eq!(
        finalization["product_sha256"],
        sha256(&action.product_bytes)
    );
    assert!(action.declared.leaf.qualified_product.is_none());
    assert!(!action.declared.leaf.directory.join("qualified").exists());
}

#[test]
fn owned_synthetic_rlib_closes_before_issuing_compilation_data() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    assert!(action.declared.leaf.metadata_full.is_none());
    let members = action.expected.leaf.files["source-tree"]
        .directory_members
        .as_ref()
        .unwrap();
    assert_eq!(
        members.keys().cloned().collect::<Vec<_>>(),
        vec![PathBuf::from("source.rs")]
    );
    assert_eq!(
        members[&PathBuf::from("source.rs")].sha256,
        sha256(b"pub fn fixture_source() {}\n")
    );
    assert_eq!(
        action.declared.leaf.input_roots["declared:source-tree"],
        action.declared.leaf.source_tree
    );
    assert_compilation(fixture.run(&action), &action);
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_eq!(fixture.runner.commands[0].status, 0);
    let record: Value =
        serde_json::from_slice(&fs::read(&action.declared.leaf.evidence).unwrap()).unwrap();
    assert_eq!(record["role"], "support");
    assert_eq!(record["producer"], "fixture//:support");
    assert_eq!(record["wrapper_status"], 0);
    assert!(record.get("qualification").is_none());
}

#[test]
fn a_second_owned_rlib_rebinds_and_consumes_the_closed_declared_dependency() {
    let mut fixture = Fixture::new();
    let dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
    // Author the consumer expectations BEFORE dynamic producer outputs exist.
    let consumer = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
    assert!(!dependency.declared.leaf.evidence.exists());
    let companion = &consumer.expected.leaf.files["role/support/artifact/evidence"];
    assert_eq!(
        companion.produced.as_ref().unwrap().output,
        ProducerOutput::Evidence
    );
    assert!(companion.sha256.is_empty());
    assert_compilation(fixture.run(&dependency), &dependency);
    assert!(consumer.declared.leaf.metadata_full.is_none());
    assert_eq!(
        consumer.declared.dependencies["support"]
            .metadata_full
            .as_ref(),
        Some(&dependency.declared.leaf.product)
    );
    refresh_inventory(&consumer);
    assert_compilation(fixture.run(&consumer), &consumer);
    assert_eq!(fixture.runner.commands.len(), 2);
    assert!(consumer.product_bytes.ends_with(&dependency.product_bytes));
    let record: Value =
        serde_json::from_slice(&fs::read(&consumer.declared.leaf.evidence).unwrap()).unwrap();
    assert_eq!(
        record["dependencies"]["declared:dependency-code"],
        "support"
    );
}

#[test]
fn relocated_dependency_uses_retained_original_spellings_and_current_declared_files() {
    let mut fixture = Fixture::new();
    let mut dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
    assert_compilation(fixture.run(&dependency), &dependency);
    let original_product = dependency.declared.leaf.product.clone();
    let original_evidence = fs::read(&dependency.declared.leaf.evidence).unwrap();
    let original_inventory = fs::read(&dependency.declared.leaf.input_inventory).unwrap();
    let relocated = fixture.root.join("relocated");
    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        fs::create_dir(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.path().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    copy_tree(&dependency.home, &relocated);
    let relocate = |path: &PathBuf| {
        path.strip_prefix(&dependency.home)
            .map(|relative| relocated.join(relative))
            .unwrap_or_else(|_| path.clone())
    };
    let producer = &mut dependency.declared.leaf;
    for map in [
        &mut producer.files,
        &mut producer.declared_inputs,
        &mut producer.input_roots,
        &mut producer.consumed_inputs,
        &mut producer.linked_inputs,
        &mut producer.control_inputs,
    ] {
        for path in map.values_mut() {
            *path = relocate(path);
        }
    }
    for path in [
        &mut producer.source_tree,
        &mut producer.recipe,
        &mut producer.product,
        &mut producer.input_inventory,
        &mut producer.invocation,
        &mut producer.evidence,
        &mut producer.finalization,
        &mut producer.directory,
    ] {
        *path = relocate(path);
    }
    fs::remove_dir_all(&dependency.home).unwrap();
    assert!(!original_product.exists());
    assert_eq!(fs::read(&producer.evidence).unwrap(), original_evidence);
    assert_eq!(
        fs::read(&producer.input_inventory).unwrap(),
        original_inventory
    );
    let record: Value = serde_json::from_slice(&original_evidence).unwrap();
    assert_eq!(
        record["original_product"],
        json!(original_product.as_os_str().as_bytes())
    );
    assert_ne!(producer.product, original_product);
    let consumer = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
    assert_compilation(fixture.run(&consumer), &consumer);
    assert_eq!(
        fixture.runner.commands.len(),
        2,
        "retained dependency reran its compiler"
    );
}

#[test]
fn declared_role_producer_configuration_and_selection_swaps_refuse_before_execution() {
    for change in 0..4 {
        let mut fixture = Fixture::new();
        let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
        match change {
            0 => action.declared.leaf.role = "other".into(),
            1 => action.declared.leaf.producer = "fixture//:other".into(),
            2 => action.declared.leaf.configuration = "another-full-configuration".into(),
            3 => action.declared.leaf.selection = "metadata".into(),
            _ => unreachable!(),
        }
        let error = refused(fixture.run(&action));
        assert!(error.contains("differs"), "{error}");
        assert!(fixture.runner.commands.is_empty());
        assert_unfinalized(&action);
    }
}

#[test]
fn same_byte_rlib_at_another_path_is_not_the_declared_dependency_product() {
    let mut fixture = Fixture::new();
    let dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
    assert_compilation(fixture.run(&dependency), &dependency);
    let mut consumer = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
    let substitute = consumer.home.join("same-bytes.rlib");
    fs::copy(&dependency.declared.leaf.product, &substitute).unwrap();
    consumer
        .declared
        .leaf
        .files
        .insert("dependency-code".into(), substitute);
    let error = refused(fixture.run(&consumer));
    assert!(error.contains("consumed code product"), "{error}");
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_unfinalized(&consumer);
}

#[test]
fn retained_dependency_data_cannot_change_identity_with_repaired_checksums() {
    for field in ["role", "producer", "configuration"] {
        let mut fixture = Fixture::new();
        let dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
        assert_compilation(fixture.run(&dependency), &dependency);
        let mut evidence: Value =
            serde_json::from_slice(&fs::read(&dependency.declared.leaf.evidence).unwrap()).unwrap();
        evidence[field] = json!("wrong-independent-identity");
        let evidence = serde_json::to_vec(&evidence).unwrap();
        fs::write(&dependency.declared.leaf.evidence, &evidence).unwrap();
        let mut finalization: Value =
            serde_json::from_slice(&fs::read(&dependency.declared.leaf.finalization).unwrap())
                .unwrap();
        finalization["evidence_sha256"] = json!(sha256(&evidence));
        fs::write(
            &dependency.declared.leaf.finalization,
            serde_json::to_vec(&finalization).unwrap(),
        )
        .unwrap();
        let consumer = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
        let error = refused(fixture.run(&consumer));
        assert!(error.contains("retained native"), "{error}");
        assert_eq!(fixture.runner.commands.len(), 1);
        assert_unfinalized(&consumer);
    }
}

#[test]
fn a_matching_leaf_does_not_cover_wrong_dependency_features_or_profile() {
    for change_profile in [false, true] {
        let mut fixture = Fixture::new();
        let mut dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
        // Reserve JSON whitespace before the successful producer. Later
        // adversarial cohort edits keep these input sizes unchanged, so its
        // original executor inventory still reaches the compiler-cohort guard.
        for path in [
            &dependency.declared.leaf.recipe,
            &dependency.declared.leaf.control_inputs["expected"],
        ] {
            let mut bytes = fs::read(path).unwrap();
            bytes.resize(bytes.len() + 512, b' ');
            fs::write(path, bytes).unwrap();
        }
        let recipe_size = fs::metadata(&dependency.declared.leaf.recipe)
            .unwrap()
            .len() as usize;
        let manifest_size = fs::metadata(&dependency.declared.leaf.control_inputs["expected"])
            .unwrap()
            .len() as usize;
        refresh_inventory(&dependency);
        assert_compilation(fixture.run(&dependency), &dependency);
        if !change_profile {
            dependency
                .expected
                .leaf
                .features
                .push("different_dependency_feature".into());
            // Keep the new declared recipe and expectation aligned so the
            // retained compiler's unchanged feature cohort remains the refusal
            // boundary. The handoff and successful record are not rewritten.
            let mut recipe: Value =
                serde_json::from_slice(&fs::read(&dependency.declared.leaf.recipe).unwrap())
                    .unwrap();
            recipe["declared_features"] = json!(dependency.expected.leaf.features);
            let mut bytes = serde_json::to_vec(&recipe).unwrap();
            assert!(bytes.len() <= recipe_size);
            bytes.resize(recipe_size, b' ');
            fs::write(&dependency.declared.leaf.recipe, bytes).unwrap();
        }
        let mut consumer =
            fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
        let expected_dependency = consumer.expected.dependencies.get_mut("support").unwrap();
        if change_profile {
            expected_dependency.profile.opt_level = "2".into();
        }
        // Author the changed independent control without changing the observed
        // retained handoff or its original executor inventory.
        let mut retained_expected = dependency.expected.clone();
        retained_expected.leaf = consumer.expected.dependencies["support"].clone();
        let mut bytes = serde_json::to_vec(&retained_expected).unwrap();
        assert!(bytes.len() <= manifest_size);
        bytes.resize(manifest_size, b' ');
        fs::write(&dependency.declared.leaf.control_inputs["expected"], bytes).unwrap();
        seal_expected(&consumer);
        refresh_inventory(&consumer);
        let error = refused(fixture.run(&consumer));
        assert!(error.contains("cohort"), "{error}");
        assert_eq!(
            fixture.runner.commands.len(),
            1,
            "leaf ran before dependency refusal"
        );
        assert_unfinalized(&consumer);
    }
}

#[test]
fn typed_factory_binds_the_complete_expected_control_graph_before_execution() {
    for case in [
        "matching",
        "foreign",
        "missing",
        "mismatch",
        "retained-foreign",
    ] {
        let mut fixture = Fixture::new();
        let dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
        // Independent actions each own the same finite budget; neither an
        // active deadline nor a completed failure is extended or retried.
        let mut producer_runner = Runner::new(
            dependency.home.join("producer-logs"),
            Duration::from_secs(20),
            65536,
        )
        .unwrap();
        assert_compilation(
            with_owned_rust_action(
                &dependency.expected,
                &dependency.declared,
                &dependency.request,
                &mut producer_runner,
            ),
            &dependency,
        );
        assert_eq!(producer_runner.commands.len(), 1);
        let action = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
        let mut manifest = action.expected.clone();
        let manifest_path = if case == "retained-foreign" {
            manifest = dependency.expected.clone();
            &dependency.declared.leaf.control_inputs["expected"]
        } else {
            &action.declared.leaf.control_inputs["expected"]
        };
        match case {
            "matching" => {}
            "foreign" | "retained-foreign" => {
                let mut foreign = dependency.expected.leaf.clone();
                foreign.role = "foreign".into();
                manifest.dependencies.insert("foreign".into(), foreign);
            }
            "missing" => manifest.dependencies.clear(),
            "mismatch" => {
                manifest
                    .dependencies
                    .get_mut("support")
                    .unwrap()
                    .configuration = "foreign configuration".into();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            manifest.leaf,
            if case == "retained-foreign" {
                dependency.expected.leaf.clone()
            } else {
                action.expected.leaf.clone()
            }
        );
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        refresh_inventory(&action);
        begin_consumer_phase(&mut fixture);
        if case == "matching" {
            assert_compilation(fixture.run(&action), &action);
            assert_eq!(fixture.runner.commands.len(), 1);
            assert_eq!(
                producer_runner.commands.len() + fixture.runner.commands.len(),
                2
            );
        } else {
            let error = refused(fixture.run(&action));
            assert_eq!(
                error, "producer expected control dependency map differs from independent graph",
                "{case}"
            );
            assert_eq!(
                fixture.runner.commands.len(),
                0,
                "{case}: consumer executed"
            );
            assert_eq!(
                producer_runner.commands.len() + fixture.runner.commands.len(),
                1
            );
            assert_unfinalized(&action);
        }
    }
}

#[test]
fn wrong_handoff_compiler_profile_features_source_environment_cwd_and_product_refuse() {
    for mode in [
        "wrong-compiler",
        "wrong-profile",
        "wrong-features",
        "wrong-source",
        "wrong-environment",
        "wrong-cwd",
        "wrong-product",
        "wrong-response",
    ] {
        let mut fixture = Fixture::new();
        let action = fixture.action("support", "code-rlib-static_pic", mode, None);
        let error = refused(fixture.run(&action));
        let reason = match mode {
            "wrong-environment" => "normalized native compiler environment differs",
            "wrong-response" => {
                "native invocation response requires final LF and unambiguous LF lines"
            }
            _ => "normalized native compiler recipe/cwd differs",
        };
        assert_eq!(error, reason, "{mode}");
        assert_eq!(
            fixture.runner.commands.len(),
            1,
            "{mode} did not reach owned wrapper"
        );
        assert_eq!(fixture.runner.commands[0].status, 0);
        assert!(action.declared.leaf.invocation.exists());
        assert_unfinalized(&action);
    }
}

#[test]
fn matching_recipe_bytes_cannot_override_independent_profile_or_features() {
    for mode in ["wrong-profile", "wrong-features"] {
        let mut fixture = Fixture::new();
        let mut action = fixture.action("support", "code-rlib-static_pic", mode, None);
        // Match the deliberately changed observation's recipe while keeping the
        // independently selected profile/features fixed. The cohort check must
        // still refuse after exact command comparison succeeds.
        if mode == "wrong-profile" {
            let option = action
                .expected
                .leaf
                .compiler_argv
                .iter_mut()
                .find(|arg| **arg == literal(b"-Copt-level=3"))
                .unwrap();
            *option = literal(b"-Copt-level=2");
        } else {
            action
                .expected
                .leaf
                .compiler_argv
                .push(literal(b"--cfg=feature=\"unexpected\""));
        }
        seal_expected(&action);
        refresh_inventory(&action);
        let error = refused(fixture.run(&action));
        assert!(
            !error.contains("recipe/cwd differs"),
            "did not reach cohort check: {error}"
        );
        assert!(error.contains("cohort"), "{mode}: {error}");
        assert_eq!(fixture.runner.commands[0].status, 0);
        assert_unfinalized(&action);
    }
}

#[test]
fn wrapper_environment_is_an_independent_pre_execution_requirement() {
    let mut fixture = Fixture::new();
    let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    action
        .expected
        .leaf
        .wrapper_environment
        .insert("LC_ALL".into(), literal(b"not-the-runner-locale"));
    seal_expected(&action);
    refresh_inventory(&action);
    let error = refused(fixture.run(&action));
    assert!(
        error.contains("wrapper effective environment differs"),
        "{error}"
    );
    assert!(fixture.runner.commands.is_empty());
    assert_unfinalized(&action);
}

#[test]
fn source_or_tool_bytes_differing_before_execution_are_not_admitted() {
    for id in ["source", "wrapper", "compiler", "response"] {
        let mut fixture = Fixture::new();
        let action = fixture.action("support", "code-rlib-static_pic", "ok", None);
        fs::write(
            &action.declared.leaf.files[id],
            b"substituted independent input",
        )
        .unwrap();
        let error = refused(fixture.run(&action));
        assert!(error.contains("independent bytes"), "{id}: {error}");
        assert!(fixture.runner.commands.is_empty());
        assert_unfinalized(&action);
    }
}

#[test]
fn changed_owned_wrapper_request_is_not_an_execution_authorization() {
    let mut fixture = Fixture::new();
    let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    action.request.argv[0] = "/unapproved/wrapper-tool".into();
    let error = refused(fixture.run(&action));
    assert!(error.contains("wrapper argv/cwd"), "{error}");
    assert!(fixture.runner.commands.is_empty());
    assert_unfinalized(&action);
}

#[test]
fn final_wrapper_failure_after_handoff_and_product_withholds_finalization() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "wrapper-failure", None);
    let error = refused(fixture.run(&action));
    assert!(
        error.starts_with(&format!(
            "{} failed (exit status: 17); logs ",
            action.request.argv[0]
        )),
        "{error}"
    );
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_eq!(fixture.runner.commands[0].status, 17);
    assert!(action.declared.leaf.invocation.exists());
    assert!(action.declared.leaf.product.exists());
    assert!(!action.declared.leaf.evidence.exists());
    assert_unfinalized(&action);
}

#[test]
fn changed_input_during_a_successful_command_fails_close_without_finalization() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "mutate-source", None);
    let error = refused(fixture.run(&action));
    assert!(error.contains("native action close"), "{error}");
    assert!(
        error.contains(action.declared.leaf.files["source"].to_str().unwrap()),
        "source close identity lost: {error}"
    );
    assert_eq!(fixture.runner.commands[0].status, 0);
    assert!(action.declared.leaf.product.exists());
    assert_unfinalized(&action);
}

#[test]
fn added_source_tree_member_during_successful_wrapper_fails_directory_close() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "add-source-member", None);
    let added = action.declared.leaf.source_tree.join("unlisted.rs");
    assert!(!added.exists());
    let source_bytes = fs::read(&action.declared.leaf.files["source"]).unwrap();
    let error = refused(fixture.run(&action));
    assert!(error.contains("native action close"), "{error}");
    assert!(
        error.contains("native tree directory entries changed"),
        "{error}"
    );
    assert!(
        error.contains(action.declared.leaf.source_tree.to_str().unwrap()),
        "{error}"
    );
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_eq!(fixture.runner.commands[0].status, 0);
    assert_eq!(
        fs::read(&action.declared.leaf.files["source"]).unwrap(),
        source_bytes
    );
    assert_eq!(
        fs::read(&added).unwrap(),
        b"pub fn unlisted_during_command() {}\n"
    );
    assert_eq!(
        fs::read(&action.declared.leaf.product).unwrap(),
        action.product_bytes
    );
    assert_unfinalized(&action);
}

#[test]
fn primary_wrapper_or_recipe_failure_preserves_the_source_close_error() {
    for mode in ["fail-and-mutate", "recipe-and-mutate"] {
        let mut fixture = Fixture::new();
        let action = fixture.action("support", "code-rlib-static_pic", mode, None);
        let error = refused(fixture.run(&action));
        assert!(error.contains("native action close"), "{mode}: {error}");
        assert!(
            error.contains(action.declared.leaf.files["source"].to_str().unwrap()),
            "source close identity lost: {error}"
        );
        let primary = error.split("; native action close:").next().unwrap();
        assert!(!primary.is_empty());
        if mode == "fail-and-mutate" {
            assert_eq!(fixture.runner.commands[0].status, 17);
            assert!(
                primary.starts_with(&format!(
                    "{} failed (exit status: 17); logs ",
                    action.request.argv[0]
                )),
                "primary wrapper failure lost: {error}"
            );
        } else {
            assert_eq!(fixture.runner.commands[0].status, 0);
            assert_eq!(
                primary, "normalized native compiler recipe/cwd differs",
                "primary recipe failure lost: {error}"
            );
        }
        assert_unfinalized(&action);
    }
}

#[test]
fn absent_product_or_preexisting_product_cannot_finalize() {
    let mut fixture = Fixture::new();
    let missing = fixture.action("missing", "code-rlib-static_pic", "missing-product", None);
    let error = refused(fixture.run(&missing));
    assert!(
        error.starts_with(&format!(
            "metadata {}: ",
            missing.declared.leaf.product.display()
        )),
        "{error}"
    );
    assert!(!missing.declared.leaf.product.exists());
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_eq!(fixture.runner.commands[0].status, 0);
    assert!(missing.declared.leaf.invocation.exists());
    assert_unfinalized(&missing);
    let stale = fixture.action("stale", "code-rlib-static_pic", "ok", None);
    fs::write(&stale.declared.leaf.product, &stale.product_bytes).unwrap();
    let before = fixture.runner.commands.len();
    let error = refused(fixture.run(&stale));
    assert_eq!(
        error,
        "owned native product already exists or is inaccessible"
    );
    assert_eq!(
        fixture.runner.commands.len(),
        before,
        "stale product reached owned execution"
    );
    assert_unfinalized(&stale);
}

#[test]
fn synthetic_non_elf_cdylib_cannot_skip_source_and_physical_qualification() {
    let mut fixture = Fixture::new();
    let action = fixture.action("plugin", "cdylib", "ok", None);
    let error = refused(fixture.run(&action));
    // This synthetic package stops at cohort selection, before ELF inspection.
    assert_eq!(
        error,
        "no-scope build has an unreviewed package/feature/test/profile cohort"
    );
    assert_eq!(fixture.runner.commands[0].status, 0);
    assert_eq!(
        fs::read(&action.declared.leaf.product).unwrap(),
        action.product_bytes
    );
    assert!(
        !action
            .declared
            .leaf
            .qualified_product
            .as_ref()
            .unwrap()
            .exists()
    );
    assert_unfinalized(&action);
    // This fixture intentionally lacks the real source catalog/cohort too. It
    // proves refusal of synthetic constructor authority, not a positive ELF
    // proof followed by a copy-failure test.
}

#[test]
fn produced_outputs_cannot_substitute_for_source_or_tool_expectations() {
    for change in 0..3 {
        let mut fixture = Fixture::new();
        let dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
        let mut consumer =
            fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
        assert_compilation(fixture.run(&dependency), &dependency);
        let companion = consumer
            .expected
            .leaf
            .files
            .get_mut("role/support/artifact/evidence")
            .unwrap();
        match change {
            0 => companion.produced.as_mut().unwrap().role = "absent".into(),
            1 => companion.source_key = Some("rv:reverie-liteinst/src/patch_alloc.rs".into()),
            2 => companion.produced.as_mut().unwrap().output = ProducerOutput::Product,
            _ => unreachable!(),
        }
        seal_expected(&consumer);
        refresh_inventory(&consumer);
        let error = refused(fixture.run(&consumer));
        assert!(error.contains("produced"), "{error}");
        assert_eq!(fixture.runner.commands.len(), 1);
        assert_unfinalized(&consumer);
    }
}

#[test]
fn produced_directory_cannot_hide_an_undeclared_companion() {
    let mut fixture = Fixture::new();
    let dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let consumer = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
    assert_compilation(fixture.run(&dependency), &dependency);
    fs::write(
        dependency.declared.leaf.directory.join("extra-receipt"),
        b"not a normal output role",
    )
    .unwrap();
    refresh_inventory(&consumer);
    let error = refused(fixture.run(&consumer));
    assert!(error.contains("companion output membership"), "{error}");
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_unfinalized(&consumer);
}

fn outputs(action: &Action) -> reverie_liteinst_artifact::native_action_input::LeafOutputs {
    let leaf = &action.declared.leaf;
    reverie_liteinst_artifact::native_action_input::LeafOutputs {
        recipe: leaf.recipe.clone(),
        product: leaf.product.clone(),
        directory: leaf.directory.clone(),
        input_inventory: leaf.input_inventory.clone(),
        invocation: leaf.invocation.clone(),
        evidence: leaf.evidence.clone(),
        finalization: leaf.finalization.clone(),
        qualified_product: leaf.qualified_product.clone(),
    }
}

#[test]
fn expected_manifest_bytes_remain_guarded_through_owned_wrapper_and_final_close() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "mutate-expected", None);
    let manifest = action.home.join("expected.json");
    fs::write(&manifest, serde_json::to_vec(&action.expected).unwrap()).unwrap();
    refresh_inventory(&action);
    let error = refused(
        reverie_liteinst_artifact::native_action::with_owned_rust_action_files(
            &manifest,
            &outputs(&action),
            &action.request,
            &mut fixture.runner,
        ),
    );
    assert!(error.contains("native action close"), "{error}");
    assert!(error.contains("expected.json"), "{error}");
    assert_eq!(fixture.runner.commands[0].status, 0);
    assert_unfinalized(&action);
}

#[test]
fn thin_cli_owns_the_synthetic_wrapper_and_emits_only_closed_compilation_data() {
    let fixture = Fixture::new();
    let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let owner = fs::canonicalize(env!("CARGO_BIN_EXE_reverie-native-rust-action")).unwrap();
    action
        .declared
        .leaf
        .control_inputs
        .insert("owner".into(), owner.clone());
    action.expected.leaf.wrapper_environment.insert(
        "RUST_ACTION_INPUT_METADATA".into(),
        literal(action.request.executor_metadata.as_os_str().as_bytes()),
    );
    seal_fixture_recipe(
        &mut action.expected.leaf,
        &mut action.declared.leaf,
        &action.declared.dependencies,
        &action.expected.dependencies,
    );
    seal_expected(&action);
    refresh_inventory(&action);
    let manifest = action.home.join("expected.json");
    fs::write(&manifest, serde_json::to_vec(&action.expected).unwrap()).unwrap();
    let leaf = &action.declared.leaf;
    let result = std::process::Command::new(&owner)
        .current_dir(&action.request.cwd)
        .env(
            "RUST_ACTION_INPUT_METADATA",
            &action.request.executor_metadata,
        )
        .args([
            "--expected-inputs",
            manifest.to_str().unwrap(),
            "--action-inputs",
            leaf.recipe.to_str().unwrap(),
            "--output-directory",
            leaf.directory.to_str().unwrap(),
            "--invocation",
            leaf.invocation.to_str().unwrap(),
            "--evidence",
            leaf.evidence.to_str().unwrap(),
            "--finalization",
            leaf.finalization.to_str().unwrap(),
            "--input-inventory",
            leaf.input_inventory.to_str().unwrap(),
            "--product",
            leaf.product.to_str().unwrap(),
            "--",
        ])
        .args(&action.request.argv)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["kind"], "compilation");
    assert_eq!(fs::read(&leaf.product).unwrap(), action.product_bytes);
    assert!(leaf.finalization.is_file());
    assert!(!leaf.directory.join("qualified").exists());
}

#[test]
fn relative_output_cannot_change_meaning_between_factory_and_owned_wrapper() {
    let mut fixture = Fixture::new();
    let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let original = action.declared.leaf.product.to_str().unwrap().to_owned();
    action.declared.leaf.product = PathBuf::from("product.rlib");
    for argument in &mut action.request.argv {
        if *argument == original {
            *argument = "product.rlib".into();
        }
    }
    let error = refused(fixture.run(&action));
    assert!(error.contains("working directories to match"), "{error}");
    assert!(fixture.runner.commands.is_empty());
    assert_unfinalized(&action);
}

#[test]
fn whole_factory_retains_complete_inventory_including_nested_inputs_and_controls() {
    let mut fixture = Fixture::new();
    let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let nested = action.home.join("hidden-tree/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("hidden-input"),
        b"independently authored hidden bytes",
    )
    .unwrap();
    let root = nested.parent().unwrap().to_path_buf();
    action
        .expected
        .leaf
        .files
        .insert("hidden-tree".into(), expected_input(&root));
    action
        .declared
        .leaf
        .declared_inputs
        .insert("hidden-tree".into(), root.clone());
    action
        .declared
        .leaf
        .input_roots
        .insert("declared:hidden-tree".into(), root);
    seal_fixture_recipe(
        &mut action.expected.leaf,
        &mut action.declared.leaf,
        &action.declared.dependencies,
        &action.expected.dependencies,
    );
    seal_expected(&action);
    refresh_inventory(&action);
    let inventory_bytes = fs::read(&action.request.executor_metadata).unwrap();
    let inventory: Value = serde_json::from_slice(&inventory_bytes).unwrap();
    let rows: BTreeSet<_> = inventory["digests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| PathBuf::from(row["path"].as_str().unwrap()))
        .collect();
    for required in [
        &nested.join("hidden-input"),
        &action.declared.leaf.files["python"],
        &action.declared.leaf.files["response"],
        &action.declared.leaf.files["source"],
        &action.declared.leaf.recipe,
        &action.declared.leaf.control_inputs["owner"],
        &action.declared.leaf.control_inputs["expected"],
    ] {
        assert!(
            rows.contains(
                fs::canonicalize(required)
                    .unwrap()
                    .strip_prefix("/")
                    .unwrap()
            )
        );
    }
    assert!(!rows.contains(action.request.executor_metadata.strip_prefix("/").unwrap()));
    assert_compilation(fixture.run(&action), &action);
    assert_eq!(
        fs::read(&action.declared.leaf.input_inventory).unwrap(),
        inventory_bytes
    );
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_eq!(fixture.runner.commands[0].status, 0);
}

#[test]
fn executor_inventory_missing_or_extra_regular_row_refuses_before_wrapper() {
    for extra in [false, true] {
        let mut fixture = Fixture::new();
        let action = fixture.action("support", "code-rlib-static_pic", "ok", None);
        let mut inventory: Value =
            serde_json::from_slice(&fs::read(&action.request.executor_metadata).unwrap()).unwrap();
        let rows = inventory["digests"].as_array_mut().unwrap();
        if extra {
            rows.push(json!({"path":"unbound/extra-regular-file","digest":format!("{}:31", "b".repeat(40))}));
        } else {
            let source = action.declared.leaf.files["source"]
                .strip_prefix("/")
                .unwrap();
            let index = rows
                .iter()
                .position(|row| row["path"] == json!(source))
                .unwrap();
            rows.remove(index);
        }
        fs::write(
            &action.request.executor_metadata,
            serde_json::to_vec(&inventory).unwrap(),
        )
        .unwrap();
        let error = refused(fixture.run(&action));
        assert!(error.contains("complete regular-file path"), "{error}");
        assert!(fixture.runner.commands.is_empty());
        assert!(!action.declared.leaf.product.exists());
        assert_unfinalized(&action);
    }
}

#[test]
fn executor_metadata_changed_by_successful_wrapper_fails_close() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "mutate-metadata", None);
    let before = fs::read(&action.request.executor_metadata).unwrap();
    let error = refused(fixture.run(&action));
    assert!(error.contains("native action close"), "{error}");
    assert!(
        error.contains(action.request.executor_metadata.to_str().unwrap()),
        "{error}"
    );
    assert_eq!(fixture.runner.commands.len(), 1);
    assert_eq!(fixture.runner.commands[0].status, 0);
    let mut changed = before.clone();
    changed.push(b' ');
    assert_eq!(
        fs::read(&action.request.executor_metadata).unwrap(),
        changed
    );
    assert_eq!(
        fs::read(&action.declared.leaf.input_inventory).unwrap(),
        before
    );
    assert_eq!(
        fs::read(&action.declared.leaf.product).unwrap(),
        action.product_bytes
    );
    assert_unfinalized(&action);
}

#[test]
fn wrong_owner_or_expected_control_association_refuses_before_wrapper() {
    for control in ["owner", "expected"] {
        let mut fixture = Fixture::new();
        let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
        let actual_expected = action.declared.leaf.control_inputs["expected"].clone();
        let replacement = action.home.join(format!("other-{control}"));
        if control == "expected" {
            fs::copy(&actual_expected, &replacement).unwrap();
        } else {
            fs::write(&replacement, b"not the running owner executable").unwrap();
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        }
        action
            .declared
            .leaf
            .control_inputs
            .insert(control.into(), replacement);
        seal_fixture_recipe(
            &mut action.expected.leaf,
            &mut action.declared.leaf,
            &action.declared.dependencies,
            &action.expected.dependencies,
        );
        refresh_inventory(&action);
        let error = refused(
            reverie_liteinst_artifact::native_action::with_owned_rust_action_files(
                &actual_expected,
                &outputs(&action),
                &action.request,
                &mut fixture.runner,
            ),
        );
        assert!(error.contains(control), "{control}: {error}");
        assert!(fixture.runner.commands.is_empty());
        assert!(!action.declared.leaf.product.exists());
        assert_unfinalized(&action);
    }
}

#[test]
fn nonexecutable_compiler_bytes_do_not_satisfy_the_tool_contract() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let compiler = &action.declared.leaf.files["compiler"];
    let bytes = fs::read(compiler).unwrap();
    fs::set_permissions(compiler, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        sha256(&fs::read(compiler).unwrap()),
        action.expected.leaf.files["compiler"].sha256
    );
    let error = refused(fixture.run(&action));
    assert!(error.contains("executable"), "{error}");
    assert_eq!(fs::read(compiler).unwrap(), bytes);
    assert_unfinalized(&action);
}

#[test]
fn old_version_two_recipe_cannot_use_executor_inventory_as_upgrade() {
    let mut fixture = Fixture::new();
    let action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let mut recipe: Value =
        serde_json::from_slice(&fs::read(&action.declared.leaf.recipe).unwrap()).unwrap();
    recipe["version"] = json!(2);
    fs::write(
        &action.declared.leaf.recipe,
        serde_json::to_vec(&recipe).unwrap(),
    )
    .unwrap();
    refresh_inventory(&action);
    let error = refused(fixture.run(&action));
    assert!(error.contains("recipe version"), "{error}");
    assert!(fixture.runner.commands.is_empty());
    assert_unfinalized(&action);
}

#[test]
fn cli_context_slots_bind_real_child_environment_and_refuse_wrong_compiler_values() {
    // Real CLI and wrapper processes, synthetic handoff/product only. No Rust
    // compilation or constructor qualification is claimed by this control.
    let fixture = Fixture::new();
    let owner = fs::canonicalize(env!("CARGO_BIN_EXE_reverie-native-rust-action")).unwrap();
    let slots = [
        ("TMPDIR", ContextSlot::Tmpdir),
        ("PWD", ContextSlot::Pwd),
        ("EXECUTION_ID", ContextSlot::ExecutionId),
        ("SESSION_ID", ContextSlot::SessionId),
        ("ACTION_DIGEST", ContextSlot::ActionDigest),
        ("BUCK_SCRATCH_PATH", ContextSlot::BuckScratchPath),
    ];
    for (name, mode, absent_optional) in [
        ("context-first", "ok", false),
        ("context-second", "ok", false),
        ("context-absent", "ok", true),
        ("context-wrong", "wrong-compiler-context", false),
        ("context-extra", "extra-compiler-context", false),
    ] {
        let mut action = fixture.action(name, "code-rlib-static_pic", mode, None);
        let temporary = action.home.join("context-tmp");
        fs::create_dir(&temporary).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child_environment = BTreeMap::from([
            ("TMPDIR", temporary.to_str().unwrap().to_owned()),
            ("PWD", format!("/logical/{name}/working-directory")),
        ]);
        if !absent_optional {
            let scratch = action.home.join("context-scratch");
            fs::create_dir(&scratch).unwrap();
            fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
            child_environment.extend([
                ("EXECUTION_ID", format!("{name}-execution")),
                ("SESSION_ID", format!("{name}-session")),
                ("ACTION_DIGEST", format!("{name}-opaque-action-digest")),
                ("BUCK_SCRATCH_PATH", scratch.to_str().unwrap().to_owned()),
            ]);
        }
        action
            .declared
            .leaf
            .control_inputs
            .insert("owner".into(), owner.clone());
        action.expected.leaf.wrapper_environment.insert(
            "RUST_ACTION_INPUT_METADATA".into(),
            literal(action.request.executor_metadata.as_os_str().as_bytes()),
        );
        for (key, slot) in slots {
            let reference = vec![ArgumentPart::ExecutionContext { slot }];
            action
                .expected
                .leaf
                .wrapper_environment
                .insert(key.into(), reference.clone());
            // The normal wrapper consumes scratch itself; it does not forward
            // BUCK_SCRATCH_PATH into the compiler environment.
            if slot != ContextSlot::BuckScratchPath {
                action
                    .expected
                    .leaf
                    .compiler_environment
                    .insert(key.into(), reference);
            }
        }
        let config_path = action.declared.leaf.files["config"].clone();
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["context_keys"] = json!([
            "TMPDIR",
            "PWD",
            "EXECUTION_ID",
            "SESSION_ID",
            "ACTION_DIGEST"
        ]);
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        action
            .expected
            .leaf
            .files
            .insert("config".into(), expected_input(&config_path));
        seal_fixture_recipe(
            &mut action.expected.leaf,
            &mut action.declared.leaf,
            &action.declared.dependencies,
            &action.expected.dependencies,
        );
        seal_expected(&action);
        refresh_inventory(&action);
        let leaf = &action.declared.leaf;
        let mut command = std::process::Command::new(&owner);
        command.current_dir(&action.request.cwd);
        // Each child owns this map. Neither the parallel test process's cwd nor
        // its environment changes, including when an ambient optional ID exists.
        for (key, _) in slots {
            command.env_remove(key);
        }
        command.envs(&child_environment);
        command.env(
            "RUST_ACTION_INPUT_METADATA",
            &action.request.executor_metadata,
        );
        command
            .args([
                "--expected-inputs",
                leaf.control_inputs["expected"].to_str().unwrap(),
                "--action-inputs",
                leaf.recipe.to_str().unwrap(),
                "--output-directory",
                leaf.directory.to_str().unwrap(),
                "--invocation",
                leaf.invocation.to_str().unwrap(),
                "--evidence",
                leaf.evidence.to_str().unwrap(),
                "--finalization",
                leaf.finalization.to_str().unwrap(),
                "--input-inventory",
                leaf.input_inventory.to_str().unwrap(),
                "--product",
                leaf.product.to_str().unwrap(),
                "--",
            ])
            .args(&action.request.argv);
        let result = command.output().unwrap();
        let invocation = reverie_liteinst_artifact::native_invocation::parse_invocation_handoff(
            &fs::read(&leaf.invocation).unwrap(),
        )
        .unwrap();
        let compiler_environment: BTreeMap<_, _> = invocation.env.into_iter().collect();
        assert_eq!(invocation.cwd, b"/");
        assert_eq!(
            compiler_environment[b"FIXTURE_MODE".as_slice()],
            b"synthetic-adapter-only"
        );
        assert!(!compiler_environment.contains_key(b"BUCK_SCRATCH_PATH".as_slice()));
        assert_eq!(fs::read(&leaf.product).unwrap(), action.product_bytes);
        if mode == "ok" {
            assert!(
                result.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let value: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(value["kind"], "compilation");
            let expected_compiler: BTreeMap<_, _> = child_environment
                .iter()
                .filter(|(key, _)| **key != "BUCK_SCRATCH_PATH")
                .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
                .chain(std::iter::once((
                    b"FIXTURE_MODE".to_vec(),
                    b"synthetic-adapter-only".to_vec(),
                )))
                .collect();
            assert_eq!(compiler_environment, expected_compiler);
            let record: Value = serde_json::from_slice(&fs::read(&leaf.evidence).unwrap()).unwrap();
            assert_eq!(record["wrapper_status"], 0);
            assert_eq!(record["context"]["cwd"], json!(b"/"));
            for (key, slot) in slots {
                let slot_name = serde_json::to_value(slot).unwrap();
                let expected_value = child_environment
                    .get(key)
                    .map(|value| value.as_bytes().to_vec());
                assert_eq!(
                    record["context"]["slots"][slot_name.as_str().unwrap()],
                    json!(expected_value)
                );
            }
            assert!(leaf.finalization.is_file());
        } else {
            assert!(
                !result.status.success(),
                "{name}: wrong compiler context succeeded"
            );
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(
                error.contains("compiler environment differs"),
                "{name}: {error}"
            );
            if mode == "wrong-compiler-context" {
                assert_ne!(
                    compiler_environment[b"EXECUTION_ID".as_slice()],
                    child_environment["EXECUTION_ID"].as_bytes()
                );
            } else {
                assert_eq!(
                    compiler_environment[b"UNEXPECTED_COMPILER_CONTEXT".as_slice()],
                    b"extra"
                );
            }
            assert!(!leaf.evidence.exists());
            assert_unfinalized(&action);
        }
        assert!(!leaf.directory.join("qualified").exists());
    }
}

// Pin the real host executable before it produces anything. An owned copy
// permits mutation controls without changing the installed/tested executable.
// The Python compiler stand-in remains synthetic: this is owner/custody proof,
// never Rust compiler execution or physical ELF qualification.
fn distinct_cli_owner(action: &mut Action, pinned: bool) -> PathBuf {
    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_reverie-native-rust-action")).unwrap();
    let bytes = fs::read(executable).unwrap();
    let digest = sha256(&bytes);
    assert_ne!(
        digest,
        sha256(&fs::read(std::env::current_exe().unwrap()).unwrap())
    );
    let owner = action.home.join("retained-producer-owner");
    fs::write(&owner, &bytes).unwrap();
    fs::set_permissions(&owner, fs::Permissions::from_mode(0o755)).unwrap();
    action
        .declared
        .leaf
        .control_inputs
        .insert("owner".into(), owner.clone());
    action.expected.leaf.owner_pin = pinned.then_some(OwnerPin {
        sha256: digest,
        size: bytes.len() as u64,
    });
    action.expected.leaf.wrapper_environment.insert(
        "RUST_ACTION_INPUT_METADATA".into(),
        literal(action.request.executor_metadata.as_os_str().as_bytes()),
    );
    seal_fixture_recipe(
        &mut action.expected.leaf,
        &mut action.declared.leaf,
        &action.declared.dependencies,
        &action.expected.dependencies,
    );
    seal_expected(action);
    refresh_inventory(action);
    owner
}

fn execute_distinct_cli_producer(action: &Action, owner: &std::path::Path) {
    let leaf = &action.declared.leaf;
    assert!(!leaf.finalization.exists());
    let result = std::process::Command::new(owner)
        .current_dir(&action.request.cwd)
        .env(
            "RUST_ACTION_INPUT_METADATA",
            &action.request.executor_metadata,
        )
        .args([
            "--expected-inputs",
            leaf.control_inputs["expected"].to_str().unwrap(),
            "--action-inputs",
            leaf.recipe.to_str().unwrap(),
            "--output-directory",
            leaf.directory.to_str().unwrap(),
            "--invocation",
            leaf.invocation.to_str().unwrap(),
            "--evidence",
            leaf.evidence.to_str().unwrap(),
            "--finalization",
            leaf.finalization.to_str().unwrap(),
            "--input-inventory",
            leaf.input_inventory.to_str().unwrap(),
            "--product",
            leaf.product.to_str().unwrap(),
            "--",
        ])
        .args(&action.request.argv)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["kind"], "compilation");
    assert_eq!(value["reused"], false);
    assert_eq!(fs::read(&leaf.product).unwrap(), action.product_bytes);
    assert!(leaf.finalization.is_file());
    assert!(!leaf.directory.join("qualified").exists());
}

fn begin_consumer_phase(fixture: &mut Fixture) {
    // The independent CLI producer did not use this Runner. Start the same
    // finite consumer budget after producer completion and inventory setup.
    assert!(fixture.runner.commands.is_empty());
    fixture.runner = Runner::new(
        fixture.root.join("consumer-logs"),
        Duration::from_secs(20),
        65536,
    )
    .unwrap();
}

#[test]
fn a_distinct_pinned_cli_producer_rebinds_in_the_downstream_executable() {
    let mut fixture = Fixture::new();
    let mut dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let owner = distinct_cli_owner(&mut dependency, true);
    // Both roles' independent expectations precede producer output/records.
    let consumer = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
    assert_eq!(
        consumer.expected.dependencies["support"].owner_pin,
        dependency.expected.leaf.owner_pin
    );
    assert!(consumer.expected.leaf.owner_pin.is_none());
    execute_distinct_cli_producer(&dependency, &owner);
    let retained = [
        &dependency.declared.leaf.recipe,
        &dependency.declared.leaf.control_inputs["expected"],
        &dependency.declared.leaf.invocation,
        &dependency.declared.leaf.evidence,
        &dependency.declared.leaf.finalization,
        &dependency.declared.leaf.input_inventory,
    ]
    .map(|path| (path, fs::read(path).unwrap()));
    let record: Value =
        serde_json::from_slice(&fs::read(&dependency.declared.leaf.evidence).unwrap()).unwrap();
    assert_eq!(
        record["control_sha256"]["$control:owner"],
        dependency.expected.leaf.owner_pin.as_ref().unwrap().sha256
    );
    assert!(fixture.runner.commands.is_empty());
    refresh_inventory(&consumer);
    begin_consumer_phase(&mut fixture);
    assert_compilation(fixture.run(&consumer), &consumer);
    assert_eq!(
        fixture.runner.commands.len(),
        1,
        "retained producer was executed again"
    );
    assert!(consumer.product_bytes.ends_with(&dependency.product_bytes));
    for (path, bytes) in retained {
        assert_eq!(
            fs::read(path).unwrap(),
            bytes,
            "consumer rewrote retained producer data"
        );
    }
}

#[test]
fn an_unpinned_distinct_cli_producer_is_not_an_authorized_retained_owner() {
    let mut fixture = Fixture::new();
    let mut dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let owner = distinct_cli_owner(&mut dependency, false);
    execute_distinct_cli_producer(&dependency, &owner);
    let mut consumer = fixture.action("consumer", "code-rlib-static_pic", "ok", Some(&dependency));
    let executing_owner = fs::read(std::env::current_exe().unwrap()).unwrap();
    consumer.expected.leaf.owner_pin = Some(OwnerPin {
        sha256: sha256(&executing_owner),
        size: executing_owner.len() as u64,
    });
    assert_eq!(
        fs::read(&consumer.declared.leaf.control_inputs["owner"]).unwrap(),
        executing_owner
    );
    seal_expected(&consumer);
    refresh_inventory(&consumer);
    assert!(
        consumer.expected.dependencies["support"]
            .owner_pin
            .is_none()
    );
    assert!(consumer.expected.leaf.owner_pin.is_some());
    begin_consumer_phase(&mut fixture);
    let error = refused(fixture.run(&consumer));
    assert!(
        error.contains("declared owner control is not the executing factory"),
        "{error}"
    );
    assert!(fixture.runner.commands.is_empty());
    assert!(!consumer.declared.leaf.product.exists());
    assert_unfinalized(&consumer);
}

#[test]
fn pinned_foreign_owner_does_not_authorize_the_current_executing_leaf() {
    let mut fixture = Fixture::new();
    let mut action = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let owner = distinct_cli_owner(&mut action, true);
    assert_eq!(
        sha256(&fs::read(&owner).unwrap()),
        action.expected.leaf.owner_pin.as_ref().unwrap().sha256
    );
    // This process is the executing factory. A correct foreign byte pin cannot
    // excuse declaring that different executable as the owner of this action.
    let error = refused(fixture.run(&action));
    assert!(
        error.contains("declared owner control is not the executing factory"),
        "{error}"
    );
    assert!(fixture.runner.commands.is_empty());
    assert!(!action.declared.leaf.product.exists());
    assert_unfinalized(&action);
}

#[test]
fn changed_pinned_producer_owner_refuses_before_execution_and_at_close() {
    for during_wrapper in [false, true] {
        let mut fixture = Fixture::new();
        let mut dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
        let owner = distinct_cli_owner(&mut dependency, true);
        execute_distinct_cli_producer(&dependency, &owner);
        let mut consumer = fixture.action(
            "consumer",
            "code-rlib-static_pic",
            if during_wrapper {
                "mutate-source"
            } else {
                "ok"
            },
            Some(&dependency),
        );
        if during_wrapper {
            // Reuse the fixture's explicit mutation operation against the owned
            // producer executable. The compiler's source operand stays intact.
            let config_path = &consumer.declared.leaf.files["config"];
            let mut config: Value =
                serde_json::from_slice(&fs::read(config_path).unwrap()).unwrap();
            config["source"] = json!(owner);
            fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            for id in ["config", "declared:config"] {
                consumer
                    .expected
                    .leaf
                    .files
                    .insert(id.into(), expected_input(config_path));
            }
            seal_expected(&consumer);
        } else {
            let mut bytes = fs::read(&owner).unwrap();
            // Keep length unchanged, so this reaches the independent digest
            // comparison rather than merely failing a size/inventory check.
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(&owner, bytes).unwrap();
        }
        refresh_inventory(&consumer);
        begin_consumer_phase(&mut fixture);
        let error = refused(fixture.run(&consumer));
        if during_wrapper {
            assert!(
                error.contains("native action close") && error.contains("retained-producer-owner"),
                "{error}"
            );
            assert_eq!(fixture.runner.commands.len(), 1);
            assert_eq!(fixture.runner.commands[0].status, 0);
            assert_eq!(
                fs::read(&consumer.declared.leaf.product).unwrap(),
                consumer.product_bytes
            );
        } else {
            assert!(
                error.contains("native owner control differs from independent bytes"),
                "{error}"
            );
            assert!(fixture.runner.commands.is_empty());
            assert!(!consumer.declared.leaf.product.exists());
        }
        assert_unfinalized(&consumer);
    }
}

#[test]
fn cli_selected_child_environment_strips_unrelated_owner_keys_without_relaxing_consumed_values() {
    // Real owner/Runner/Python processes, but only synthetic handoff and product
    // bytes. Every expected value below is authored before either child runs.
    // No process-global environment mutation or native compilation occurs.
    let fixture = Fixture::new();
    let owner = fs::canonicalize(env!("CARGO_BIN_EXE_reverie-native-rust-action")).unwrap();
    let slots = [
        ("TMPDIR", ContextSlot::Tmpdir),
        ("PWD", ContextSlot::Pwd),
        ("EXECUTION_ID", ContextSlot::ExecutionId),
        ("SESSION_ID", ContextSlot::SessionId),
        ("ACTION_DIGEST", ContextSlot::ActionDigest),
        ("BUCK_SCRATCH_PATH", ContextSlot::BuckScratchPath),
    ];
    // The normal wrapper's literal inherited names and four prefix patterns.
    // The fixture explicitly forwards the present inherited values through its
    // synthetic compiler handoff; startup controls remain wrapper-only.
    let inherited_names: BTreeSet<String> = [
        "RUSTC_LOG",
        "RUST_BACKTRACE",
        "PATH",
        "PWD",
        "HOME",
        "RUSTUP_HOME",
        "TMPDIR",
        "LOCALAPPDATA",
        "PROGRAMDATA",
        "TEMP",
        "TMP",
        "EXECUTION_ID",
        "SESSION_ID",
        "ACTION_DIGEST",
        "RE_PLATFORM",
        "CAS_DAEMON_PORT",
        "CAS_DAEMON_ADDR",
        "USER",
        "DOTSLASH_CACHE",
        "SYSTEMROOT",
        "USERNAME",
        "CHGDISABLE",
        "NIX_BINTOOLS",
        "NIX_BINTOOLS_FOR_TARGET",
        "NIX_CC",
        "NIX_CC_FOR_TARGET",
        "NIX_CFLAGS_COMPILE",
        "NIX_CFLAGS_COMPILE_FOR_TARGET",
        "NIX_COREFOUNDATION_RPATH",
        "NIX_DEBUG",
        "NIX_DONT_SET_RPATH",
        "NIX_DONT_SET_RPATH_FOR_BUILD",
        "NIX_ENFORCE_NO_NATIVE",
        "NIX_HARDENING_ENABLE",
        "NIX_IGNORE_LD_THROUGH_GCC",
        "NIX_LDFLAGS",
        "NIX_LDFLAGS_FOR_TARGET",
        "NIX_NO_SELF_RPATH",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let inherited_prefixes: BTreeSet<String> = [
        "NIX_BINTOOLS_WRAPPER_TARGET_HOST_",
        "NIX_BINTOOLS_WRAPPER_TARGET_TARGET_",
        "NIX_CC_WRAPPER_TARGET_HOST_",
        "NIX_CC_WRAPPER_TARGET_TARGET_",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for (name, mode, absent_optional, changed_owner) in [
        ("selected-first", "ok", false, None),
        ("selected-direct", "ok", false, None),
        ("selected-absent", "ok", true, None),
        (
            "selected-path",
            "ok",
            false,
            Some(("PATH", "/wrong/toolchain")),
        ),
        ("selected-home", "ok", false, Some(("HOME", "/wrong/home"))),
        ("selected-lang", "ok", false, Some(("LANG", "C"))),
        (
            "selected-python-buffering",
            "ok",
            false,
            Some(("PYTHONUNBUFFERED", "0")),
        ),
        (
            "selected-extra-inherited",
            "ok",
            false,
            Some(("RUSTC_LOG", "unmodeled=debug")),
        ),
        (
            "selected-extra-nix",
            "ok",
            false,
            Some((
                "NIX_BINTOOLS_WRAPPER_TARGET_TARGET_unmodeled",
                "/unmodeled/nix",
            )),
        ),
        (
            "selected-extra-python",
            "ok",
            false,
            Some(("PYTHONPATH", "/unmodeled/python")),
        ),
        (
            "selected-extra-loader",
            "ok",
            false,
            Some(("LD_LIBRARY_PATH", "/unmodeled/native-libraries")),
        ),
        ("selected-script-pin", "ok", false, None),
        (
            "selected-wrong-compiler",
            "wrong-compiler-context",
            false,
            None,
        ),
        (
            "selected-extra-compiler",
            "extra-compiler-context",
            false,
            None,
        ),
    ] {
        let mut action = fixture.action(name, "code-rlib-static_pic", mode, None);
        let temporary = action.home.join("selected-tmp");
        fs::create_dir(&temporary).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).unwrap();
        let inherited_values: BTreeMap<String, String> = [
            ("PATH", "/independently-fixed/toolchain"),
            ("HOME", "/independently-fixed/home"),
            ("USER", "synthetic-wrapper-user"),
            ("CHGDISABLE", "1"),
            ("RUST_BACKTRACE", "0"),
            (
                "NIX_CC_WRAPPER_TARGET_HOST_x86_64_unknown_linux_gnu",
                "/independently-fixed/nix-wrapper",
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
        let mut child_environment = inherited_values.clone();
        child_environment.extend([
            ("LANG".into(), "C.UTF-8".into()),
            ("PYTHONUNBUFFERED".into(), "1".into()),
            ("LC_ALL".into(), "C".into()),
            ("TMPDIR".into(), temporary.to_str().unwrap().into()),
            (
                "RUST_ACTION_INPUT_METADATA".into(),
                action.request.executor_metadata.to_str().unwrap().into(),
            ),
        ]);
        if !absent_optional {
            let scratch = action.home.join("selected-scratch");
            fs::create_dir(&scratch).unwrap();
            fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
            child_environment.extend([
                ("PWD".into(), format!("/logical/{name}/working-directory")),
                ("EXECUTION_ID".into(), format!("{name}-execution")),
                ("SESSION_ID".into(), format!("{name}-session")),
                (
                    "ACTION_DIGEST".into(),
                    format!("{name}-opaque-action-digest"),
                ),
                (
                    "BUCK_SCRATCH_PATH".into(),
                    scratch.strip_prefix("/").unwrap().to_str().unwrap().into(),
                ),
            ]);
        }
        action.expected.leaf.wrapper_environment = child_environment
            .iter()
            .map(|(key, value)| (key.clone(), literal(value.as_bytes())))
            .collect();
        action.expected.leaf.wrapper_environment.insert(
            "RUST_ACTION_INPUT_METADATA".into(),
            artifact("$executor_metadata"),
        );
        action.expected.leaf.compiler_environment = inherited_values
            .iter()
            .map(|(key, value)| (key.clone(), literal(value.as_bytes())))
            .collect();
        action
            .expected
            .leaf
            .compiler_environment
            .insert("FIXTURE_MODE".into(), literal(b"synthetic-adapter-only"));
        for (key, slot) in slots {
            let reference = vec![ArgumentPart::ExecutionContext { slot }];
            action
                .expected
                .leaf
                .wrapper_environment
                .insert(key.into(), reference.clone());
            if slot != ContextSlot::BuckScratchPath {
                action
                    .expected
                    .leaf
                    .compiler_environment
                    .insert(key.into(), reference);
            }
        }
        action.expected.leaf.wrapper_environment_contract = Some(PythonWrapperEnvironment {
            launcher: EnvironmentTool {
                input: "python".into(),
                member: None,
            },
            script: EnvironmentTool {
                input: if name == "selected-script-pin" {
                    "config"
                } else {
                    "wrapper"
                }
                .into(),
                member: None,
            },
            inherited_names: inherited_names.clone(),
            inherited_prefixes: inherited_prefixes.clone(),
        });
        if name == "selected-direct" {
            assert_eq!(action.request.argv.remove(1), "-B");
            assert_eq!(action.expected.leaf.wrapper_argv.remove(1), literal(b"-B"));
        }
        action
            .declared
            .leaf
            .control_inputs
            .insert("owner".into(), owner.clone());
        let config_path = action.declared.leaf.files["config"].clone();
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["expected_wrapper_environment"] = json!(child_environment);
        config["context_keys"] = json!(
            action
                .expected
                .leaf
                .compiler_environment
                .keys()
                .filter(|key| key.as_str() != "FIXTURE_MODE")
                .collect::<Vec<_>>()
        );
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        action
            .expected
            .leaf
            .files
            .insert("config".into(), expected_input(&config_path));
        seal_fixture_recipe(
            &mut action.expected.leaf,
            &mut action.declared.leaf,
            &action.declared.dependencies,
            &action.expected.dependencies,
        );
        seal_expected(&action);
        refresh_inventory(&action);

        // These are inherited by the owner only. Neither value is added to the
        // expected child, fixture assertion or compiler handoff expectation.
        let mut owner_environment = child_environment.clone();
        owner_environment.insert(
            "UNRELATED_AGENT_NOTE".into(),
            "owner-only-agent-value".into(),
        );
        owner_environment.insert(
            "UNRELATED_TELEMETRY_KEY".into(),
            "owner-only-telemetry-value".into(),
        );
        if let Some((key, value)) = changed_owner {
            owner_environment.insert(key.into(), value.into());
        }
        let leaf = &action.declared.leaf;
        let mut command = std::process::Command::new(&owner);
        command
            .current_dir(&action.request.cwd)
            .env_clear()
            .envs(&owner_environment);
        command
            .args([
                "--expected-inputs",
                leaf.control_inputs["expected"].to_str().unwrap(),
                "--action-inputs",
                leaf.recipe.to_str().unwrap(),
                "--output-directory",
                leaf.directory.to_str().unwrap(),
                "--invocation",
                leaf.invocation.to_str().unwrap(),
                "--evidence",
                leaf.evidence.to_str().unwrap(),
                "--finalization",
                leaf.finalization.to_str().unwrap(),
                "--input-inventory",
                leaf.input_inventory.to_str().unwrap(),
                "--product",
                leaf.product.to_str().unwrap(),
                "--",
            ])
            .args(&action.request.argv);
        let result = command.output().unwrap();
        let error = String::from_utf8_lossy(&result.stderr);
        let before_wrapper_refusal = changed_owner.is_some() || name == "selected-script-pin";
        if before_wrapper_refusal {
            assert!(
                !result.status.success(),
                "{name}: unbound wrapper environment succeeded"
            );
            assert!(
                result.stdout.is_empty(),
                "{name}: refusal emitted a success result"
            );
            assert!(
                error.contains("environment") || error.contains("contract"),
                "{name}: {error}"
            );
            assert!(
                !leaf.invocation.exists(),
                "{name}: refused wrapper was executed"
            );
            assert!(
                !leaf.product.exists(),
                "{name}: refused wrapper produced output"
            );
            assert!(!leaf.evidence.exists());
            assert_unfinalized(&action);
            assert!(!leaf.directory.join("qualified").exists());
            continue;
        }
        let invocation = reverie_liteinst_artifact::native_invocation::parse_invocation_handoff(
            &fs::read(&leaf.invocation).unwrap(),
        )
        .unwrap();
        let actual_compiler: BTreeMap<_, _> = invocation.env.into_iter().collect();
        let expected_compiler: BTreeMap<Vec<u8>, Vec<u8>> = child_environment
            .iter()
            .filter(|(key, _)| {
                inherited_names.contains(*key)
                    || inherited_prefixes
                        .iter()
                        .any(|prefix| key.starts_with(prefix.as_str()))
            })
            .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .chain(std::iter::once((
                b"FIXTURE_MODE".to_vec(),
                b"synthetic-adapter-only".to_vec(),
            )))
            .collect();
        assert_eq!(invocation.cwd, b"/");
        assert!(!actual_compiler.contains_key(b"BUCK_SCRATCH_PATH".as_slice()));
        assert_eq!(fs::read(&leaf.product).unwrap(), action.product_bytes);
        if mode == "ok" {
            assert!(result.status.success(), "{name}: {error}");
            let value: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(value["kind"], "compilation");
            assert_eq!(actual_compiler, expected_compiler);
            let record: Value = serde_json::from_slice(&fs::read(&leaf.evidence).unwrap()).unwrap();
            let expected_wrapper: BTreeMap<_, _> = child_environment
                .iter()
                .map(|(key, value)| (key.clone(), value.as_bytes().to_vec()))
                .collect();
            assert_eq!(record["wrapper_environment"], json!(expected_wrapper));
            for key in ["UNRELATED_AGENT_NOTE", "UNRELATED_TELEMETRY_KEY"] {
                assert!(record["wrapper_environment"].get(key).is_none());
                assert!(!actual_compiler.contains_key(key.as_bytes()));
            }
            assert_eq!(record["wrapper_status"], 0);
            assert_eq!(record["context"]["cwd"], json!(b"/"));
            for (key, slot) in slots {
                let slot_name = serde_json::to_value(slot).unwrap();
                let expected_value = child_environment
                    .get(key)
                    .map(|value| value.as_bytes().to_vec());
                assert_eq!(
                    record["context"]["slots"][slot_name.as_str().unwrap()],
                    json!(expected_value)
                );
            }
            assert!(leaf.finalization.is_file());
        } else {
            assert!(
                !result.status.success(),
                "{name}: changed compiler handoff succeeded"
            );
            assert!(result.stdout.is_empty());
            assert!(
                error.contains("compiler environment differs"),
                "{name}: {error}"
            );
            let mut corrupted = expected_compiler;
            if mode == "wrong-compiler-context" {
                corrupted
                    .get_mut(b"EXECUTION_ID".as_slice())
                    .unwrap()
                    .extend_from_slice(b".wrong");
            } else {
                corrupted.insert(b"UNEXPECTED_COMPILER_CONTEXT".to_vec(), b"extra".to_vec());
            }
            assert_eq!(actual_compiler, corrupted);
            assert!(!leaf.evidence.exists());
            assert_unfinalized(&action);
        }
        assert!(!leaf.directory.join("qualified").exists());
    }
}

// Private-static controls use the same ordinary owned wrapper transaction.
// Their products deliberately contain no ar/ELF/TLS bytes: compilation data
// alone must never grant the downstream archive/source callback.
fn reseal_static_fixture(action: &mut Action) {
    assert!(!action.declared.leaf.invocation.exists());
    assert!(!action.declared.leaf.evidence.exists());
    let mut paths = action.declared.leaf.files.clone();
    paths.insert("$product".into(), action.declared.leaf.product.clone());
    paths.insert("$directory".into(), action.declared.leaf.directory.clone());
    let mut response = Vec::new();
    for argument in &action.expected.leaf.compiler_argv[1..] {
        response.extend(render(argument, &paths));
        response.push(b'\n');
    }
    let response_path = &action.declared.leaf.files["response"];
    fs::write(response_path, response).unwrap();
    action
        .expected
        .leaf
        .files
        .insert("response".into(), expected_input(response_path));
    seal_fixture_recipe(
        &mut action.expected.leaf,
        &mut action.declared.leaf,
        &action.declared.dependencies,
        &action.expected.dependencies,
    );
    seal_expected(action);
    if action
        .declared
        .dependencies
        .values()
        .all(|producer| producer.finalization.exists())
    {
        refresh_inventory(action);
    }
}

fn static_fixture(fixture: &Fixture, mode: &str, dependency: Option<&Action>) -> Action {
    let mut action = fixture.action("detcore-liteinst", "staticlib", mode, dependency);
    action.expected.leaf.features = [
        "exec-native-injection",
        "exec-private-init",
        "loader-private-runtime",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    action
        .expected
        .leaf
        .compiler_argv
        .retain(|argument| *argument != literal(b"--cfg=feature=\"fixture_feature\""));
    for feature in &action.expected.leaf.features {
        action
            .expected
            .leaf
            .compiler_argv
            .push(literal(format!("--cfg=feature=\"{feature}\"")));
    }
    action
        .expected
        .leaf
        .compiler_argv
        .push(literal(b"--target=x86_64-unknown-linux-musl"));
    let prefix = "synthetic staticlib stand-in; neither an archive nor Rust output\n";
    action.product_bytes = prefix.as_bytes().to_vec();
    if let Some(dependency) = dependency {
        action
            .product_bytes
            .extend_from_slice(&dependency.product_bytes);
    }
    let config_path = &action.declared.leaf.files["config"];
    let mut config: Value = serde_json::from_slice(&fs::read(config_path).unwrap()).unwrap();
    config["product_prefix"] = json!(prefix);
    fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    action
        .expected
        .leaf
        .files
        .insert("config".into(), expected_input(config_path));
    reseal_static_fixture(&mut action);
    action
}

fn assert_static_compilation(result: Result<OwnedProduct>, action: &Action) {
    match result.unwrap_or_else(|error| panic!("synthetic staticlib transaction failed: {error}")) {
        OwnedProduct::Compilation(compilation) => {
            assert!(!compilation.reused());
            assert_eq!(compilation.product().sha256, sha256(&action.product_bytes));
            assert_eq!(compilation.product().path, action.declared.leaf.product);
            assert_eq!(compilation.evidence().path, action.declared.leaf.evidence);
        }
        OwnedProduct::Constructor(_) => panic!("staticlib stand-in became constructor authority"),
        OwnedProduct::StandaloneConstructor(_) => {
            panic!("staticlib stand-in became standalone authority")
        }
        OwnedProduct::NativeEntry(_) => {
            panic!("staticlib stand-in became explicit-entry authority")
        }
    }
    assert_eq!(
        fs::read(&action.declared.leaf.product).unwrap(),
        action.product_bytes
    );
    let evidence = fs::read(&action.declared.leaf.evidence).unwrap();
    let finalization: Value =
        serde_json::from_slice(&fs::read(&action.declared.leaf.finalization).unwrap()).unwrap();
    assert_eq!(finalization["evidence_sha256"], sha256(&evidence));
    assert_eq!(
        finalization["product_sha256"],
        sha256(&action.product_bytes)
    );
    assert!(action.declared.leaf.qualified_product.is_none());
    assert!(action.declared.leaf.metadata_full.is_none());
    assert!(!action.declared.leaf.directory.join("qualified").exists());
}

#[test]
fn private_static_owned_product_is_only_compilation_with_both_target_spellings() {
    for separated_target in [false, true] {
        let mut fixture = Fixture::new();
        let mut action = static_fixture(&fixture, "ok", None);
        if separated_target {
            assert_eq!(
                action.expected.leaf.compiler_argv.pop().unwrap(),
                literal(b"--target=x86_64-unknown-linux-musl"),
            );
            action
                .expected
                .leaf
                .compiler_argv
                .extend([literal(b"--target"), literal(b"x86_64-unknown-linux-musl")]);
            reseal_static_fixture(&mut action);
        }
        let recipe: Value =
            serde_json::from_slice(&fs::read(&action.declared.leaf.recipe).unwrap()).unwrap();
        assert_eq!(recipe["selection"], "staticlib");
        assert_eq!(recipe["crate_type"], "staticlib");
        assert_eq!(recipe["emit"], "link");
        assert_static_compilation(fixture.run(&action), &action);
        assert_eq!(fixture.runner.commands.len(), 1);
        assert_eq!(fixture.runner.commands[0].status, 0);
    }
}

#[test]
fn private_static_independent_cohort_refuses_before_wrapper() {
    for case in 0..8 {
        let mut fixture = Fixture::new();
        let mut action = static_fixture(&fixture, "ok", None);
        match case {
            0 => {
                action.expected.leaf.role = "wrong-static-owner".into();
                action.declared.leaf.role = "wrong-static-owner".into();
            }
            1 => action.expected.leaf.crate_name = "wrong_static_crate".into(),
            2 => {
                action.expected.leaf.features.remove(1);
            }
            3 => {
                action.expected.leaf.profile.opt_level = "2".into();
            }
            4 => {
                action.expected.leaf.profile.debuginfo = 1;
            }
            5 => {
                action.expected.leaf.profile.debug_assertions = false;
            }
            6 => {
                action.expected.leaf.profile.overflow_checks = false;
            }
            7 => {
                action.expected.leaf.profile.test = true;
            }
            _ => unreachable!(),
        };
        reseal_static_fixture(&mut action);
        let error = refused(fixture.run(&action));
        assert!(
            error.contains("private-static producer requires the exact staticlib cohort"),
            "case {case}: {error}"
        );
        assert!(
            fixture.runner.commands.is_empty(),
            "case {case} executed the wrapper"
        );
        assert!(!action.declared.leaf.invocation.exists());
        assert_unfinalized(&action);
    }
}

#[test]
fn private_static_missing_wrong_duplicate_and_operand_targets_refuse_after_handoff() {
    for case in 0..4 {
        let mut fixture = Fixture::new();
        let mut action = static_fixture(&fixture, "ok", None);
        assert_eq!(
            action.expected.leaf.compiler_argv.pop().unwrap(),
            literal(b"--target=x86_64-unknown-linux-musl")
        );
        let replacement = match case {
            0 => Vec::new(),
            1 => vec![literal(b"--target=x86_64-unknown-linux-gnu")],
            2 => vec![literal(b"--target=x86_64-unknown-linux-musl"); 2],
            3 => vec![
                literal(b"--remap-path-prefix"),
                literal(b"--target=x86_64-unknown-linux-musl"),
            ],
            _ => unreachable!(),
        };
        action.expected.leaf.compiler_argv.extend(replacement);
        reseal_static_fixture(&mut action);
        let error = refused(fixture.run(&action));
        assert!(
            error.contains("private-static compiler target differs from the selected musl target"),
            "case {case}: {error}"
        );
        assert_eq!(fixture.runner.commands.len(), 1);
        assert_eq!(fixture.runner.commands[0].status, 0);
        assert!(action.declared.leaf.invocation.is_file());
        assert!(!action.declared.leaf.evidence.exists());
        assert_unfinalized(&action);
    }
}

#[test]
fn private_static_hidden_dependency_and_metadata_do_not_supply_code_coverage() {
    let mut fixture = Fixture::new();
    let dependency = fixture.action("support", "code-rlib-static_pic", "ok", None);
    let mut action = static_fixture(&fixture, "ok", Some(&dependency));
    action.expected.leaf.dependencies.clear();
    action.declared.leaf.dependencies.clear();
    action.declared.leaf.linked_inputs.clear();
    assert!(!action.expected.leaf.metadata_dependencies.is_empty());
    assert!(!action.declared.leaf.consumed_inputs.is_empty());
    assert!(action.expected.dependencies.contains_key("support"));
    assert!(
        action
            .declared
            .leaf
            .files
            .keys()
            .any(|id| id.starts_with("role/support/artifact/"))
    );
    // Keep the independent expectation explicit. The code-coverage gate runs
    // before any nonexistent producer output or executor inventory is read.
    seal_expected(&action);
    let error = refused(fixture.run(&action));
    assert!(
        error.contains("actual leaf code-product coverage"),
        "{error}"
    );
    assert!(fixture.runner.commands.is_empty());
    assert!(!action.declared.leaf.invocation.exists());
    assert_unfinalized(&action);
}

#[test]
fn private_static_public_scope_refuses_ordinary_and_rlib_before_recipe_reads() {
    for selection in ["cdylib", "code-rlib-static_pic"] {
        let mut fixture = Fixture::new();
        let action = fixture.action("detcore-liteinst", selection, "ok", None);
        fs::remove_file(&action.declared.leaf.recipe).unwrap();
        let output = fixture.root.join("private-link-output");
        let mut called = false;
        let result = with_declared_private_static(
            &action.expected,
            &action.declared,
            &output,
            &mut fixture.runner,
            |_, _, _| {
                called = true;
                Ok(())
            },
        );
        let error = result.expect_err("ordinary or rlib source reached private-static callback");
        assert!(
            error.contains("private-static producer requires the exact staticlib cohort"),
            "{selection}: {error}"
        );
        assert!(!called);
        assert!(fixture.runner.commands.is_empty());
        assert!(!output.exists());
        assert_unfinalized(&action);
    }
}

#[test]
fn private_static_retained_compilation_cannot_supply_missing_source_authority() {
    let mut fixture = Fixture::new();
    let action = static_fixture(&fixture, "ok", None);
    assert_static_compilation(fixture.run(&action), &action);
    let marker = fs::read(&action.declared.leaf.finalization).unwrap();
    let output = fixture.root.join("private-link-output");
    let mut called = false;
    let result = with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |_, _, _| {
            called = true;
            Ok(())
        },
    );
    let error =
        result.expect_err("compilation-only stand-in gained private-static source authority");
    assert!(error.contains("private-static source role is missing: workspace:detcore-liteinst/src/exec_initial/mapped_entry.rs"), "{error}");
    assert!(!called);
    assert_eq!(
        fixture.runner.commands.len(),
        1,
        "retained refusal ran a command"
    );
    assert!(!output.exists());
    assert_eq!(
        fs::read(&action.declared.leaf.finalization).unwrap(),
        marker
    );
    assert_eq!(
        fs::read(&action.declared.leaf.product).unwrap(),
        action.product_bytes
    );
}

#[test]
fn private_static_source_label_cannot_substitute_its_configured_owner() {
    let mut fixture = Fixture::new();
    let mut action = static_fixture(&fixture, "ok", None);
    let entry = action.expected.leaf.files.get_mut("source").unwrap();
    entry.source_key = Some("workspace:detcore-liteinst/src/exec_initial/mapped_entry.rs".into());
    entry.compiler_filename = Some(artifact("source"));
    // Deliberately mislabel independently pinned fixture bytes: the static
    // source owner check must reject before any physical source/TLS premise.
    action
        .expected
        .leaf
        .files
        .get_mut("config")
        .unwrap()
        .source_key = Some("rv:reverie-inguest/Cargo.toml".into());
    reseal_static_fixture(&mut action);
    assert_static_compilation(fixture.run(&action), &action);
    let output = fixture.root.join("private-link-output");
    let mut called = false;
    let result = with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |_, _, _| {
            called = true;
            Ok(())
        },
    );
    let error = result.expect_err("wrong configured source owner reached static callback");
    assert!(error.contains("private-static source role belongs to a different configured producer: rv:reverie-inguest/Cargo.toml"), "{error}");
    assert!(!called);
    assert_eq!(fixture.runner.commands.len(), 1);
    assert!(!output.exists());
    assert!(action.declared.leaf.finalization.is_file());
}

#[test]
fn private_static_primary_and_late_source_failure_both_withhold_finalization() {
    for mode in ["fail-and-mutate", "recipe-and-mutate"] {
        let mut fixture = Fixture::new();
        let action = static_fixture(&fixture, mode, None);
        let error = refused(fixture.run(&action));
        assert!(error.contains("native action close"), "{mode}: {error}");
        assert!(
            error.contains(action.declared.leaf.files["source"].to_str().unwrap()),
            "source close failure lost: {error}"
        );
        let primary = error.split("; native action close:").next().unwrap();
        if mode == "fail-and-mutate" {
            assert_eq!(fixture.runner.commands[0].status, 17);
            assert!(
                primary.contains("17"),
                "primary wrapper failure lost: {error}"
            );
        } else {
            assert_eq!(fixture.runner.commands[0].status, 0);
            assert!(
                primary.contains("compiler recipe"),
                "primary recipe failure lost: {error}"
            );
        }
        assert_eq!(fixture.runner.commands.len(), 1);
        assert!(!action.declared.leaf.evidence.exists());
        assert_unfinalized(&action);
        assert!(!action.declared.leaf.directory.join("qualified").exists());
    }
}

struct StaticFixtureSource {
    relative: &'static str,
    bytes: &'static [u8],
    size: u64,
    sha256: &'static str,
    compiled: bool,
}

macro_rules! fixed_static_sources {
    ($(($path:literal, $size:literal, $sha:literal, $compiled:literal)),+ $(,)?) => {
        const STATIC_FIXTURE_SOURCES: &[StaticFixtureSource] = &[
            $(StaticFixtureSource {
                relative: $path,
                bytes: include_bytes!(concat!("../../", $path)),
                size: $size,
                sha256: $sha,
                compiled: $compiled,
            }),+
        ];
    };
}

// Independent fixed fixture declarations. The first ten files are exactly the
// existing SHARED_INPUTS_051 TLS tuple; no production hash/tuple is broadened.
// The remaining roles exercise complete source ownership and tree associations.
fixed_static_sources! {
    ("reverie-inguest/Cargo.toml", 5569, "71e2a295a5f59e3c129247c91b9beb84d19a07b0148153c884449af37d082aa7", false),
    ("reverie-inguest/src/lib.rs", 11734, "0fbf94503eac27b65e52967c11c675e23e7a2f50338ca32150002ff4c64fabbf", true),
    ("reverie-inguest/src/private_runtime/mod.rs", 2093, "f6764939d2016bd3c9414b17b35f21f8cf7217f128135b83cfb5459aa3ae5e1e", true),
    ("reverie-inguest/src/private_runtime/tls/mod.rs", 4853, "369e35bcafda51a271720507dcf7a7c9072fb5db7486e110e14f15e4df91f0dd", true),
    ("reverie-inguest/src/private_runtime/tls/abi.rs", 12171, "f7de3cd3a0653785f8aacc01e0fabdeb89172b2660f07ea0df9d393078299aab", true),
    ("reverie-inguest/src/private_runtime/tls/layout.rs", 499, "8fcceedb234c081b5125d95d73eaafa083fadc2d1be41698b724487ab7cfae36", true),
    ("reverie-loader/Cargo.toml", 343, "781863e2fb39d08ba64776020dc859abe6b68024f4c4219dc6fb96f4b814b203", false),
    ("reverie-loader/src/lib.rs", 1510, "b3318334635e632a23283c1f51633b090eb809de9710ab0c98038b684abbb5b8", true),
    ("reverie-loader/src/musl_tls.rs", 7478, "84011bb8505fa3357908e7445b1305f54303bae32ddb9d393546da8e309ee1ea", true),
    ("reverie-loader/src/mapped_inputs.rs", 17803, "81cd7474215ff617fb1bc16b236461d4deb33697c7d559130f4bdb98b0f0e2c2", true),
    ("reverie-loader/src/handoff.rs", 76132, "85abd905256f3bc5b526840cf3cd5e090238e4aa770f03d320c2ae907ee7ae75", true),
    ("reverie-inguest/src/guest/exec_private.rs", 182321, "026cecaff9850b3bf916bc0ecb56d0e9021ca5130e7bfc1f64e21a48bc0dd4a9", true),
    ("reverie-inguest/src/guest/exec_private/mapped_initial.rs", 37162, "a28b8e9b8a8b3efdc70f39e11253a5525aff36c315d33cd3fbec99f0489845ad", true),
    ("reverie-inguest/src/guest/exec_private/mapped_initial/mapped_scalar.rs", 20465, "285f7f75bf9f388a771d685607ce65f7097c27d21869952d46b3a7ce70897612", true),
    ("reverie-inguest/src/guest/exec_private/mapped_initial/libc_relay.rs", 50822, "a7a4df88f1fd44855a0ccf7c60ca03dedb8c7e336c6239549484ee038fbc9547", true),
    ("reverie-inguest/src/guest/exec_private/mapped_initial/libc_relay/frontend.rs", 18922, "28a785bd1945b7aae1e72bc688da1918368ab5e2fb1a4d9927bf00be5e01fcb4", true),
    ("reverie-inguest/src/trap.rs", 49799, "5c9249419574f00c540dec767359181c8ac03a47aa2df0f19e5cef0a59b5755b", true),
    ("reverie-inguest/src/trap/frame.rs", 35075, "357d2d884f7fdcc43b8d4acde8bc04e43023c85e6a77c569ac60c64dbb6030a0", true),
    ("reverie-inguest/src/trap/fixed_gates.rs", 36554, "ff3f6f6c46e872734ae921e869dbccc0ce1aeefe4bc3af8f29363cf276501e54", true),
}

fn add_static_source_roles(action: &mut Action) {
    let mut members = action.expected.leaf.files["source-tree"]
        .directory_members
        .as_ref()
        .unwrap()
        .clone();
    for (index, source) in STATIC_FIXTURE_SOURCES.iter().enumerate() {
        assert_eq!(
            source.bytes.len() as u64,
            source.size,
            "{}",
            source.relative
        );
        assert_eq!(sha256(source.bytes), source.sha256, "{}", source.relative);
        if source.relative.split('/').next().unwrap() != action.expected.leaf.role {
            continue;
        }
        let id = format!("static-source-{index}");
        let path = action.declared.leaf.source_tree.join(source.relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, source.bytes).unwrap();
        let mut spec = ExpectedFile {
            produced: None,
            directory_members: None,
            sha256: source.sha256.into(),
            size: source.size,
            source_key: None,
            compiler_filename: None,
        };
        // Tree inventory expectations are independently authored from the fixed
        // bytes; source authority belongs only to this producing role's input.
        members.insert(PathBuf::from(source.relative), spec.clone());
        spec.source_key = Some(format!("rv:{}", source.relative));
        spec.compiler_filename = source.compiled.then(|| artifact(&id));
        action.expected.leaf.files.insert(id.clone(), spec);
        action
            .declared
            .leaf
            .declared_inputs
            .insert(id.clone(), path.clone());
        action
            .declared
            .leaf
            .input_roots
            .insert(format!("declared:{id}"), path);
    }
    action
        .expected
        .leaf
        .files
        .get_mut("source-tree")
        .unwrap()
        .directory_members = Some(members);
    action
        .expected
        .leaf
        .compiler_argv
        .push(literal(b"--target=x86_64-unknown-linux-musl"));
    reseal_static_fixture(action);
}

fn clear_forwarded_source_authority(spec: &mut ExpectedFile) {
    spec.source_key = None;
    spec.compiler_filename = None;
    if let Some(members) = spec.directory_members.as_mut() {
        for member in members.values_mut() {
            clear_forwarded_source_authority(member);
        }
    }
}

fn complete_static_scope_fixture() -> (Fixture, Action) {
    complete_static_scope_fixture_with_external(false)
}

const EXTERNAL_LINK_ID: &str = "external-native-link";
const EXTERNAL_LINK_BYTES: &[u8] = b"independently specified synthetic external link input\n";

fn external_fixture_environment(action: &Action) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("LANG".into(), "C.UTF-8".into()),
        ("LC_ALL".into(), "C".into()),
        ("PYTHONUNBUFFERED".into(), "1".into()),
        (
            "TMPDIR".into(),
            action
                .home
                .join("external-owner-tmp")
                .to_str()
                .unwrap()
                .into(),
        ),
        (
            "RUST_ACTION_INPUT_METADATA".into(),
            action.request.executor_metadata.to_str().unwrap().into(),
        ),
    ])
}

// The fixture's normal CLI invokes the real owned factory and its unchanged
// Python-wrapper path. The wrapper only records a synthetic compiler handoff;
// neither the external bytes nor the staticlib stand-in are native artifacts.
fn configure_external_link_fixture(action: &mut Action) {
    use std::os::unix::fs::symlink;

    let package = action.home.join("external-package");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("link-input.a"), EXTERNAL_LINK_BYTES).unwrap();
    let alias = action.home.join("package-link");
    symlink(&package, &alias).unwrap();
    let requested = alias.join("link-input.a");
    action
        .declared
        .leaf
        .linked_inputs
        .insert(EXTERNAL_LINK_ID.into(), requested.clone());
    action
        .expected
        .leaf
        .external_linked_inputs
        .insert(EXTERNAL_LINK_ID.into());
    action.expected.leaf.files.insert(
        EXTERNAL_LINK_ID.into(),
        ExpectedFile {
            produced: None,
            directory_members: None,
            sha256: sha256(EXTERNAL_LINK_BYTES),
            size: EXTERNAL_LINK_BYTES.len() as u64,
            source_key: None,
            compiler_filename: None,
        },
    );
    action
        .declared
        .leaf
        .files
        .insert(EXTERNAL_LINK_ID.into(), requested.clone());
    action
        .expected
        .leaf
        .compiler_argv
        .push(composed(b"-Clink-arg=", EXTERNAL_LINK_ID, b""));

    let temporary = action.home.join("external-owner-tmp");
    fs::create_dir(&temporary).unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).unwrap();
    let environment = external_fixture_environment(action);
    action.expected.leaf.wrapper_environment = environment
        .iter()
        .map(|(name, value)| (name.clone(), literal(value.as_bytes())))
        .collect();
    action.expected.leaf.wrapper_environment_contract = Some(PythonWrapperEnvironment {
        launcher: EnvironmentTool {
            input: "python".into(),
            member: None,
        },
        script: EnvironmentTool {
            input: "wrapper".into(),
            member: None,
        },
        inherited_names: BTreeSet::new(),
        inherited_prefixes: BTreeSet::new(),
    });
    let owner_bytes = fs::read(env!("CARGO_BIN_EXE_reverie-native-rust-action")).unwrap();
    let owner = action.home.join("external-producer-owner");
    fs::write(&owner, &owner_bytes).unwrap();
    fs::set_permissions(&owner, fs::Permissions::from_mode(0o755)).unwrap();
    action
        .declared
        .leaf
        .control_inputs
        .insert("owner".into(), owner);
    action.expected.leaf.owner_pin = Some(OwnerPin {
        sha256: sha256(&owner_bytes),
        size: owner_bytes.len() as u64,
    });

    let wrapper_path = action.declared.leaf.files["wrapper"].clone();
    let needle = "print('synthetic adapter result; no Rust compiler executed')";
    assert_eq!(WRAPPER.matches(needle).count(), 1);
    let wrapper = WRAPPER.replace(needle, &format!(
        "assert pathlib.Path(c['external_input']).read_bytes() == b'independently specified synthetic external link input\\n'\nif c.get('replace_external_alias', False):\n    os.rename(c['external_alias'], c['external_alias'] + '.old')\n    os.symlink(c['external_target'], c['external_alias'])\n{needle}",
    ));
    fs::write(&wrapper_path, &wrapper).unwrap();
    action
        .expected
        .leaf
        .files
        .insert("wrapper".into(), expected_input(&wrapper_path));
    let config_path = action.declared.leaf.files["config"].clone();
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["expected_wrapper_environment"] = json!(environment);
    config["external_input"] = json!(requested);
    config["external_alias"] = json!(alias);
    config["external_target"] = json!(package);
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    action
        .expected
        .leaf
        .files
        .insert("config".into(), expected_input(&config_path));
    reseal_static_fixture(action);
}

// Model the source-backed executor File-only inventory independently of the
// expected omission set. Clearing or corrupting the expectation below does not
// cause this fixture to invent a regular row for the known external link.
fn refresh_external_fixture_inventory(action: &Action) {
    refresh_inventory(action);
    let endpoint = fs::canonicalize(&action.declared.leaf.linked_inputs[EXTERNAL_LINK_ID]).unwrap();
    let relative = endpoint.strip_prefix(&action.request.cwd).unwrap();
    let mut inventory: Value =
        serde_json::from_slice(&fs::read(&action.request.executor_metadata).unwrap()).unwrap();
    let rows = inventory["digests"].as_array_mut().unwrap();
    let before = rows.len();
    rows.retain(|row| row["path"] != json!(relative));
    assert_eq!(
        rows.len() + 1,
        before,
        "fixture external endpoint was not uniquely observed"
    );
    fs::write(
        &action.request.executor_metadata,
        serde_json::to_vec(&inventory).unwrap(),
    )
    .unwrap();
}

fn execute_external_fixture(action: &Action) -> std::process::Output {
    let leaf = &action.declared.leaf;
    assert!(!leaf.finalization.exists());
    std::process::Command::new(&leaf.control_inputs["owner"])
        .current_dir(&action.request.cwd)
        .env_clear()
        .envs(external_fixture_environment(action))
        .args([
            "--expected-inputs",
            leaf.control_inputs["expected"].to_str().unwrap(),
            "--action-inputs",
            leaf.recipe.to_str().unwrap(),
            "--output-directory",
            leaf.directory.to_str().unwrap(),
            "--invocation",
            leaf.invocation.to_str().unwrap(),
            "--evidence",
            leaf.evidence.to_str().unwrap(),
            "--finalization",
            leaf.finalization.to_str().unwrap(),
            "--input-inventory",
            leaf.input_inventory.to_str().unwrap(),
            "--product",
            leaf.product.to_str().unwrap(),
            "--",
        ])
        .args(&action.request.argv)
        .output()
        .unwrap()
}

fn assert_external_cli_compilation(action: &Action, output: std::process::Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["kind"], "compilation");
    assert_eq!(result["reused"], false);
    assert_eq!(
        fs::read(&action.declared.leaf.product).unwrap(),
        action.product_bytes
    );
    assert!(action.declared.leaf.finalization.is_file());
    assert!(action.declared.leaf.qualified_product.is_none());
    assert!(!action.declared.leaf.directory.join("qualified").exists());
    let record: Value =
        serde_json::from_slice(&fs::read(&action.declared.leaf.evidence).unwrap()).unwrap();
    assert_eq!(record["kind"], "reverie-native-compilation/v3");
    assert_eq!(record["wrapper_status"], 0);
    assert_eq!(
        record["input_sha256"][EXTERNAL_LINK_ID],
        sha256(EXTERNAL_LINK_BYTES)
    );
    let external = record["external_linked_inputs"].as_object().unwrap();
    assert_eq!(external.len(), 1);
    let witness = &external[EXTERNAL_LINK_ID];
    assert_eq!(
        witness["requested"],
        json!(
            action.declared.leaf.files[EXTERNAL_LINK_ID]
                .strip_prefix(&action.request.cwd)
                .unwrap()
        )
    );
    assert_eq!(
        witness["link"],
        json!(
            action
                .home
                .join("package-link")
                .strip_prefix(&action.request.cwd)
                .unwrap()
        )
    );
    assert_eq!(
        witness["target"],
        json!(action.home.join("external-package"))
    );
    assert_eq!(witness["remaining"], "link-input.a");
    let inventory: Value =
        serde_json::from_slice(&fs::read(&action.declared.leaf.input_inventory).unwrap()).unwrap();
    assert!(
        !inventory["digests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["path"].as_str().unwrap().ends_with("link-input.a") })
    );
    let marker: Value =
        serde_json::from_slice(&fs::read(&action.declared.leaf.finalization).unwrap()).unwrap();
    assert_eq!(
        marker["evidence_sha256"],
        sha256(&fs::read(&action.declared.leaf.evidence).unwrap())
    );
}

#[test]
fn expected_external_link_omission_uses_the_owned_staticlib_factory() {
    let fixture = Fixture::new();
    let mut action = static_fixture(&fixture, "ok", None);
    configure_external_link_fixture(&mut action);
    refresh_external_fixture_inventory(&action);
    assert_external_cli_compilation(&action, execute_external_fixture(&action));
}

#[test]
fn external_link_omission_requires_expected_identity_and_refuses_script_aliases() {
    for case in ["unmarked", "wrong-id", "wrong-pin", "script-alias"] {
        let fixture = Fixture::new();
        let mut action = static_fixture(&fixture, "ok", None);
        configure_external_link_fixture(&mut action);
        match case {
            "unmarked" => action.expected.leaf.external_linked_inputs.clear(),
            "wrong-id" => {
                action.expected.leaf.external_linked_inputs =
                    BTreeSet::from(["not-declared".into()]);
            }
            "wrong-pin" => {
                action
                    .expected
                    .leaf
                    .files
                    .get_mut(EXTERNAL_LINK_ID)
                    .unwrap()
                    .sha256 = "0".repeat(64);
            }
            _ => {
                let requested = action.declared.leaf.files[EXTERNAL_LINK_ID].clone();
                let wrapper = action.declared.leaf.files["wrapper"].clone();
                fs::write(&requested, fs::read(&wrapper).unwrap()).unwrap();
                action
                    .declared
                    .leaf
                    .declared_inputs
                    .insert("wrapper".into(), requested.clone());
                action
                    .declared
                    .leaf
                    .input_roots
                    .insert("declared:wrapper".into(), requested.clone());
                action
                    .declared
                    .leaf
                    .files
                    .insert("wrapper".into(), requested.clone());
                action.expected.leaf.files.insert(
                    EXTERNAL_LINK_ID.into(),
                    action.expected.leaf.files["wrapper"].clone(),
                );
                assert_eq!(action.request.argv[1], "-B");
                action.request.argv[2] = requested.to_str().unwrap().into();
            }
        }
        reseal_static_fixture(&mut action);
        refresh_external_fixture_inventory(&action);
        let output = execute_external_fixture(&action);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{case}: unexpected success");
        assert!(
            output.stdout.is_empty(),
            "{case}: success output on refusal"
        );
        if case == "unmarked" {
            assert!(
                error.contains("complete regular-file path sets differ"),
                "{case}: {error}"
            );
        } else if case == "wrong-pin" {
            assert!(
                error.contains("input differs from independent bytes"),
                "{case}: {error}"
            );
        } else {
            assert!(error.contains("external"), "{case}: {error}");
        }
        assert!(
            !action.declared.leaf.invocation.exists(),
            "{case}: wrapper ran"
        );
        assert!(
            !action.declared.leaf.product.exists(),
            "{case}: product appeared"
        );
        assert!(!action.declared.leaf.evidence.exists());
        assert_unfinalized(&action);
    }
}

#[test]
fn external_link_late_alias_and_source_changes_preserve_primary_failure() {
    for mode in ["ok", "fail-and-mutate"] {
        let fixture = Fixture::new();
        let mut action = static_fixture(&fixture, mode, None);
        configure_external_link_fixture(&mut action);
        let config_path = action.declared.leaf.files["config"].clone();
        let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        config["replace_external_alias"] = json!(true);
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        action
            .expected
            .leaf
            .files
            .insert("config".into(), expected_input(&config_path));
        reseal_static_fixture(&mut action);
        refresh_external_fixture_inventory(&action);
        let output = execute_external_fixture(&action);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{mode}: changed alias was admitted"
        );
        assert!(output.stdout.is_empty());
        assert!(error.contains("native action close"), "{mode}: {error}");
        assert!(
            error.contains(action.home.join("package-link").to_str().unwrap()),
            "{mode}: {error}"
        );
        assert!(action.declared.leaf.invocation.exists());
        assert!(action.declared.leaf.product.exists());
        if mode == "fail-and-mutate" {
            assert!(
                error
                    .split("; native action close:")
                    .next()
                    .unwrap()
                    .contains("17"),
                "{error}"
            );
            assert!(
                error.contains(action.declared.leaf.files["source"].to_str().unwrap()),
                "{error}"
            );
        }
        assert_unfinalized(&action);
    }
}

#[test]
fn retained_external_link_relocates_to_regular_input_without_rewriting_original_evidence() {
    let (mut fixture, mut action) = complete_static_scope_fixture_with_external(true);
    let original = action.declared.leaf.files[EXTERNAL_LINK_ID].clone();
    let current = action.home.join("regular-cache-input.a");
    fs::write(&current, EXTERNAL_LINK_BYTES).unwrap();
    let retained: Vec<_> = [
        &action.declared.leaf.recipe,
        &action.declared.leaf.evidence,
        &action.declared.leaf.input_inventory,
        &action.declared.leaf.finalization,
    ]
    .into_iter()
    .map(|path| (path.clone(), fs::read(path).unwrap()))
    .collect();
    action
        .declared
        .leaf
        .files
        .insert(EXTERNAL_LINK_ID.into(), current.clone());
    action
        .declared
        .leaf
        .linked_inputs
        .insert(EXTERNAL_LINK_ID.into(), current.clone());
    fs::remove_file(action.home.join("package-link")).unwrap();
    fs::remove_dir_all(action.home.join("external-package")).unwrap();
    assert!(!original.exists());
    assert!(fs::symlink_metadata(&current).unwrap().is_file());
    begin_consumer_phase(&mut fixture);
    let output = fixture.root.join("external-retained-consumer-output");
    let mut called = false;
    with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |source, archive, runner| {
            called = true;
            source.verify_unchanged()?;
            archive.verify()?;
            assert!(
                runner.commands.is_empty(),
                "retained admission reran a producer"
            );
            assert_eq!(archive.file().sha256, sha256(&action.product_bytes));
            Ok(())
        },
    )
    .unwrap_or_else(|error| panic!("external-to-regular retained admission failed: {error}"));
    assert!(called);
    assert!(!output.exists());

    let evidence_path = &action.declared.leaf.evidence;
    let marker_path = &action.declared.leaf.finalization;
    let evidence_bytes = fs::read(evidence_path).unwrap();
    let marker_bytes = fs::read(marker_path).unwrap();
    let mut evidence: Value = serde_json::from_slice(&evidence_bytes).unwrap();
    evidence["kind"] = json!("reverie-native-compilation/v2");
    let changed_evidence = serde_json::to_vec(&evidence).unwrap();
    let mut marker: Value = serde_json::from_slice(&marker_bytes).unwrap();
    marker["evidence_sha256"] = json!(sha256(&changed_evidence));
    fs::write(evidence_path, changed_evidence).unwrap();
    fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    let mut called = false;
    let error = with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |_, _, _| {
            called = true;
            Ok(())
        },
    )
    .unwrap_err();
    assert!(!called);
    assert!(
        error.contains("v2 native evidence cannot carry external"),
        "{error}"
    );
    fs::write(evidence_path, &evidence_bytes).unwrap();
    fs::write(marker_path, &marker_bytes).unwrap();

    // Repair the public marker hash, so only the malformed original path
    // association rejects these records; no callback authority may be issued.
    let original_relative = original
        .strip_prefix(&action.request.cwd)
        .unwrap()
        .to_str()
        .unwrap();
    let (parent, name) = original_relative.rsplit_once('/').unwrap();
    for spelling in [format!("{parent}//{name}"), format!("{parent}/./{name}")] {
        let mut evidence: Value = serde_json::from_slice(&evidence_bytes).unwrap();
        assert_eq!(
            evidence["inventory_paths"][EXTERNAL_LINK_ID],
            json!(original_relative)
        );
        assert_eq!(Path::new(&spelling), Path::new(original_relative));
        assert_ne!(
            Path::new(&spelling).as_os_str(),
            Path::new(original_relative).as_os_str()
        );
        evidence["inventory_paths"][EXTERNAL_LINK_ID] = json!(spelling);
        let changed_evidence = serde_json::to_vec(&evidence).unwrap();
        let mut marker: Value = serde_json::from_slice(&marker_bytes).unwrap();
        marker["evidence_sha256"] = json!(sha256(&changed_evidence));
        fs::write(evidence_path, changed_evidence).unwrap();
        fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
        let mut called = false;
        let error = with_declared_private_static(
            &action.expected,
            &action.declared,
            &output,
            &mut fixture.runner,
            |_, _, _| {
                called = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(!called);
        assert!(
            error.contains("original external inventory association"),
            "{spelling}: {error}"
        );
        assert!(!output.exists());
        fs::write(evidence_path, &evidence_bytes).unwrap();
        fs::write(marker_path, &marker_bytes).unwrap();
    }

    let owner = action.declared.leaf.control_inputs["owner"].clone();
    let owner_bytes = fs::read(&owner).unwrap();
    let mut changed_owner = owner_bytes.clone();
    changed_owner[0] ^= 1;
    fs::write(&owner, &changed_owner).unwrap();
    let mut called = false;
    let error = with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |_, _, _| {
            called = true;
            Ok(())
        },
    )
    .unwrap_err();
    assert!(!called);
    assert!(
        error.contains("owner control differs from independent bytes"),
        "{error}"
    );
    fs::write(&owner, &owner_bytes).unwrap();

    let source_path = action.declared.leaf.files["source"].clone();
    let mut called = false;
    let result: Result<()> = with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |_, _, _| {
            called = true;
            fs::write(&current, b"changed current external materialization").unwrap();
            fs::write(&source_path, b"changed source after retained admission").unwrap();
            Err("synthetic external retained consumer failure".into())
        },
    );
    let error = result.unwrap_err();
    assert!(called);
    assert!(
        error.contains("synthetic external retained consumer failure"),
        "{error}"
    );
    assert!(error.contains(current.to_str().unwrap()), "{error}");
    assert!(error.contains(source_path.to_str().unwrap()), "{error}");
    assert!(!output.exists());
    assert!(fixture.runner.commands.is_empty());
    for (path, bytes) in retained {
        assert_eq!(
            fs::read(path).unwrap(),
            bytes,
            "consumer rewrote original producer evidence"
        );
    }
}

fn complete_static_scope_fixture_with_external(external: bool) -> (Fixture, Action) {
    let mut fixture = Fixture::new();
    let mut producers = Vec::new();
    for role in ["reverie-inguest", "reverie-liteinst", "reverie-loader"] {
        let mut producer = fixture.action(role, "code-rlib-static_pic", "ok", None);
        add_static_source_roles(&mut producer);
        if external {
            // These dependencies execute in the test process; the leaf executes
            // in the distinct CLI. Pin the producer owner before either runs.
            let bytes = fs::read(&producer.declared.leaf.control_inputs["owner"]).unwrap();
            producer.expected.leaf.owner_pin = Some(OwnerPin {
                sha256: sha256(&bytes),
                size: bytes.len() as u64,
            });
            seal_expected(&producer);
            refresh_inventory(&producer);
        }
        producers.push(producer);
    }
    let mut action = static_fixture(&fixture, "ok", None);
    // This fixed synthetic entry is intentionally not a genuine mapped-entry
    // implementation. The callback positive cannot qualify its machine code.
    const ENTRY: &[u8] = b"pub fn fixture_source() {}\n";
    const ENTRY_SHA: &str = "b3c4af92b075836b33a8b52f596fcf3c6f92bf2794e9f035ac46b7723264c84b";
    assert_eq!(sha256(ENTRY), ENTRY_SHA);
    assert_eq!(
        fs::read(&action.declared.leaf.files["source"]).unwrap(),
        ENTRY
    );
    let entry = action.expected.leaf.files.get_mut("source").unwrap();
    assert_eq!(entry.sha256, ENTRY_SHA);
    entry.source_key = Some("workspace:detcore-liteinst/src/exec_initial/mapped_entry.rs".into());
    entry.compiler_filename = Some(artifact("source"));
    for producer in &producers {
        assert!(!producer.declared.leaf.product.exists());
        assert!(!producer.declared.leaf.finalization.exists());
        let role = producer.expected.leaf.role.clone();
        let id = format!("runtime-code-{role}");
        let consumed = format!("declared:{id}");
        let product = producer.declared.leaf.product.clone();
        let mut declared = producer.declared.leaf.clone();
        // Same exact code/metadata alias as the existing stable fixture. The
        // root still carries an explicit actual code edge for every role.
        declared.metadata_full = Some(product.clone());
        action
            .expected
            .dependencies
            .insert(role.clone(), producer.expected.leaf.clone());
        action.declared.dependencies.insert(role.clone(), declared);
        action
            .expected
            .leaf
            .dependencies
            .insert(consumed.clone(), role.clone());
        action
            .expected
            .leaf
            .metadata_dependencies
            .insert(consumed.clone(), role.clone());
        action
            .declared
            .leaf
            .dependencies
            .insert(consumed.clone(), role.clone());
        action
            .declared
            .leaf
            .metadata_dependencies
            .insert(consumed.clone(), role.clone());
        action
            .declared
            .leaf
            .declared_inputs
            .insert(id.clone(), product.clone());
        action
            .declared
            .leaf
            .input_roots
            .insert(consumed.clone(), product.clone());
        action
            .declared
            .leaf
            .consumed_inputs
            .insert(consumed.clone(), product.clone());
        action.declared.leaf.linked_inputs.insert(consumed, product);
        action.expected.leaf.files.insert(
            id.clone(),
            ExpectedFile {
                produced: Some(ProducedArtifact {
                    role: role.clone(),
                    output: ProducerOutput::Product,
                }),
                directory_members: None,
                sha256: String::new(),
                size: 0,
                source_key: None,
                compiler_filename: None,
            },
        );
        action.expected.leaf.compiler_argv.push(composed(
            format!("--extern={}=", producer.expected.leaf.crate_name).as_bytes(),
            &id,
            b"",
        ));
    }
    // Make newly declared logical paths available to the ordinary response
    // author before sealing the complete expected/current graph.
    action
        .declared
        .leaf
        .files
        .extend(action.declared.leaf.declared_inputs.clone());
    reseal_static_fixture(&mut action);
    if external {
        configure_external_link_fixture(&mut action);
    }
    for (id, spec) in &mut action.expected.leaf.files {
        if id.starts_with("role/") {
            // Forwarded bytes remain pinned; only their genuine producer may
            // attach the source role/compiler filename premise.
            clear_forwarded_source_authority(spec);
        }
    }
    seal_expected(&action);
    let independent_manifest = fs::read(&action.declared.leaf.control_inputs["expected"]).unwrap();
    assert_eq!(action.expected.dependencies.len(), 3);
    assert!(!action.declared.leaf.product.exists());
    // Each independent producer action keeps the existing twenty-second bound.
    // Its Runner starts only after all fixture source/expected manifests exist;
    // no active action receives a deadline extension or a retry.
    for producer in &producers {
        let mut runner = Runner::new(
            producer.home.join("producer-logs"),
            Duration::from_secs(20),
            65536,
        )
        .unwrap();
        assert_compilation(
            with_owned_rust_action(
                &producer.expected,
                &producer.declared,
                &producer.request,
                &mut runner,
            ),
            producer,
        );
        assert_eq!(runner.commands.len(), 1);
        assert_eq!(runner.commands[0].status, 0);
    }
    assert_eq!(
        fs::read(&action.declared.leaf.control_inputs["expected"]).unwrap(),
        independent_manifest
    );
    if external {
        refresh_external_fixture_inventory(&action);
        assert_external_cli_compilation(&action, execute_external_fixture(&action));
        assert!(fixture.runner.commands.is_empty());
    } else {
        refresh_inventory(&action);
        fixture.runner = Runner::new(
            action.home.join("producer-logs"),
            Duration::from_secs(20),
            65536,
        )
        .unwrap();
        assert_static_compilation(fixture.run(&action), &action);
        assert_eq!(fixture.runner.commands.len(), 1);
    }
    (fixture, action)
}

#[test]
fn private_static_public_scope_admits_complete_synthetic_graph_and_fixed_tls_sources() {
    let (mut fixture, action) = complete_static_scope_fixture();
    let output = fixture.root.join("public-static-link-output");
    let marker = fs::read(&action.declared.leaf.finalization).unwrap();
    let mut called = false;
    let fingerprint = with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |source, archive, runner| {
            called = true;
            source.verify_unchanged()?;
            archive.verify()?;
            assert_eq!(archive.file().path, action.declared.leaf.product);
            assert_eq!(archive.file().sha256, sha256(&action.product_bytes));
            assert_eq!(archive.evidence().path, action.declared.leaf.evidence);
            assert_eq!(source.source_sha256(), archive.source_sha256());
            assert_eq!(
                runner.commands.len(),
                1,
                "admission invoked a compiler/linker"
            );
            assert_eq!(
                fs::read(&archive.file().path).unwrap(),
                action.product_bytes
            );
            Ok(source.source_sha256().to_owned())
        },
    )
    .unwrap_or_else(|error| panic!("complete synthetic public static admission failed: {error}"));
    assert!(called);
    assert_eq!(fingerprint.len(), 64);
    assert_eq!(fixture.runner.commands.len(), 1);
    assert!(
        !output.exists(),
        "source-only callback created a link output"
    );
    assert_eq!(
        fs::read(&action.declared.leaf.finalization).unwrap(),
        marker
    );
    assert!(!action.declared.leaf.directory.join("qualified").exists());
}

#[test]
fn private_static_public_scope_preserves_callback_and_late_source_close_failure() {
    let (mut fixture, action) = complete_static_scope_fixture();
    let source_path = action.declared.dependencies["reverie-loader"]
        .source_tree
        .join("reverie-loader/src/musl_tls.rs");
    let output = fixture.root.join("public-static-link-output");
    let mut called = false;
    let result: Result<()> = with_declared_private_static(
        &action.expected,
        &action.declared,
        &output,
        &mut fixture.runner,
        |source, _, _| {
            called = true;
            source.verify_unchanged()?;
            fs::write(
                &source_path,
                b"changed after public static callback admission",
            )
            .unwrap();
            Err("synthetic consumer failure after source admission".into())
        },
    );
    let error = result.unwrap_err();
    assert!(called);
    assert!(
        error.contains("synthetic consumer failure after source admission"),
        "{error}"
    );
    assert!(
        error.contains(source_path.to_str().unwrap()),
        "late source failure lost: {error}"
    );
    assert!(error.contains("close"), "{error}");
    assert_eq!(fixture.runner.commands.len(), 1);
    assert!(!output.exists());
}

#[test]
fn private_static_public_scope_retains_partial_link_inputs_until_outer_close() {
    let (mut fixture, action) = complete_static_scope_fixture();
    let output = fixture.root.join("public-static-link-output");
    let lld = fixture.root.join("deliberately-wrong-lld");
    let missing = fixture.root.join("must-not-read-other-link-input");
    for callback_fails in [false, true] {
        fs::write(&lld, b"deliberately wrong linker; never executed").unwrap();
        let mut called = false;
        let result: Result<()> = with_declared_private_static(
            &action.expected,
            &action.declared,
            &output,
            &mut fixture.runner,
            |source, archive, runner| {
                called = true;
                let inputs = reverie_liteinst_artifact::private_static::selected_initializers::InitializerLinkInputs {
                    lld: &lld,
                    script: &missing,
                    libc: &missing,
                    supporting_archives: &[],
                    memcpy_source: &missing,
                    musl_source_authority: &missing,
                    stack_chk_source: &missing,
                    output_directory: &output,
                };
                let error =
                    match source.link_and_join_initializer_definitions(archive, &inputs, runner) {
                        Ok(_) => panic!("wrong pinned linker reached actual static link"),
                        Err(error) => error,
                    };
                assert_eq!(error, "initializer linker is not the bound July12 rust-lld");
                assert_eq!(runner.commands.len(), 1, "wrong linker was executed");
                fs::write(&lld, b"changed after refused helper returned").unwrap();
                if callback_fails {
                    Err("synthetic callback failure after partial link admission".into())
                } else {
                    Ok(())
                }
            },
        );
        let error =
            result.expect_err("late admitted link-input mutation escaped outer static close");
        assert!(called);
        assert!(
            error.contains(lld.to_str().unwrap()),
            "late linker failure lost: {error}"
        );
        assert!(error.contains("close"), "{error}");
        if callback_fails {
            assert!(
                error.contains("synthetic callback failure after partial link admission"),
                "primary callback failure lost: {error}"
            );
        }
        assert_eq!(fixture.runner.commands.len(), 1);
        assert!(!output.exists());
        assert!(!missing.exists());
    }
}

#[test]
fn private_static_public_scope_retains_streams_after_runner_replacement_and_record_failure() {
    let (fixture, action) = complete_static_scope_fixture();
    for record_write_fails in [false, true] {
        let case = if record_write_fails {
            "record-write-failure"
        } else {
            "whole-runner-replacement"
        };
        let output = fixture.root.join(format!("public-static-output-{case}"));
        let logs = fixture.root.join(format!("consumer-logs-{case}"));
        let mut consumer = Runner::new(logs.clone(), Duration::from_secs(20), 65536).unwrap();
        // A fresh Runner starts at serial zero and uses four-digit prefixes.
        // This pre-existing command record makes create_new fail only after
        // the actual child has exited and both original streams were written.
        let command_json = logs.join("0001.command.json");
        let stdout = logs.join("0001.stdout");
        let stderr = logs.join("0001.stderr");
        if record_write_fails {
            fs::write(&command_json, b"pre-existing command record").unwrap();
        }
        let mut called = false;
        let mut primary_error = None;
        let result: Result<()> = with_declared_private_static(
            &action.expected,
            &action.declared,
            &output,
            &mut consumer,
            |_, _, runner| {
                called = true;
                let command = if record_write_fails {
                    "printf original-output; printf original-error >&2"
                } else {
                    "printf original-output; printf original-error >&2; exit 7"
                };
                let primary = runner
                    .run("/bin/sh", &["-c".into(), command.into()], &fixture.root)
                    .expect_err("nonzero child or command-record creation unexpectedly succeeded");
                assert_eq!(fs::read(&stdout).unwrap(), b"original-output");
                assert_eq!(fs::read(&stderr).unwrap(), b"original-error");
                if record_write_fails {
                    assert!(primary.starts_with("create "), "{primary}");
                    assert!(
                        primary.contains(command_json.to_str().unwrap()),
                        "{primary}"
                    );
                    assert_eq!(
                        fs::read(&command_json).unwrap(),
                        b"pre-existing command record"
                    );
                } else {
                    assert!(primary.contains("failed"), "{primary}");
                    assert_eq!(runner.commands.len(), 1);
                    assert_eq!(runner.commands[0].status, 7);
                }
                fs::write(&stdout, b"late changed original stdout").unwrap();
                fs::write(&stderr, b"late changed original stderr").unwrap();
                if !record_write_fails {
                    *runner = Runner::new(
                        fixture.root.join("replacement-consumer-logs"),
                        Duration::from_secs(20),
                        65536,
                    )
                    .unwrap();
                    assert!(runner.commands.is_empty());
                }
                primary_error = Some(primary.clone());
                Err(primary)
            },
        );
        let error = result.expect_err("late command stream mutation escaped public static close");
        assert!(called, "public callback was not admitted: {error}");
        let primary = primary_error.expect("callback did not retain its primary error");
        assert!(error.contains(&primary), "primary error lost: {error}");
        if !record_write_fails {
            let diagnostic = error.to_ascii_lowercase();
            assert!(
                diagnostic.contains("runner") && diagnostic.contains("replaced"),
                "whole Runner replacement diagnostic lost: {error}"
            );
        }
        assert!(
            error.contains(stdout.to_str().unwrap()),
            "stdout close lost: {error}"
        );
        assert!(
            error.contains(stderr.to_str().unwrap()),
            "stderr close lost: {error}"
        );
        assert!(error.contains("close"), "{error}");
        assert!(!output.exists(), "failed public callback created an output");
        assert!(!action.declared.leaf.directory.join("qualified").exists());
    }
}
