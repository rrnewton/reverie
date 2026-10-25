// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Read the normal action recipe and structured typed-provider associations.
//!
//! This adapter returns data, never source or successful-build authority. Its
//! caller must obtain the bytes through the owned file transaction, retain the
//! declared producer edges, and close every input after fresh qualification.
//! Logical IDs are opaque. Original recipe spellings are only joined by those
//! IDs to current provider mappings; no recorded path is reopened here.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use super::Result;
use super::native_action::DeclaredGraph;
use super::native_action::DeclaredProducer;
use super::native_action::ExpectedGraph;
use super::native_action::ExpectedProducer;
use super::require;

const MAX_RECIPE_BYTES: usize = 16 * 1024 * 1024;

/// The selected owner's actual output CLI arguments and action-input recipe.
/// The recipe deliberately omits its own future output Artifact paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeafOutputs {
    pub recipe: PathBuf,
    pub product: PathBuf,
    pub directory: PathBuf,
    pub invocation: PathBuf,
    pub evidence: PathBuf,
    pub finalization: PathBuf,
    pub input_inventory: PathBuf,
    pub qualified_product: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct Role {
    configured_target: String,
    configuration: String,
    selection: String,
    artifacts: BTreeMap<String, PathBuf>,
    declared_inputs: BTreeMap<String, PathBuf>,
    input_roots: BTreeMap<String, PathBuf>,
    consumed_inputs: BTreeMap<String, PathBuf>,
    linked_inputs: BTreeMap<String, PathBuf>,
    control_inputs: BTreeMap<String, PathBuf>,
    roles: BTreeMap<String, Role>,
}

// Exact version-3 write_json body. In particular, there is no success field or
// current path for this producer's own output in this input recipe.
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
struct Recipe {
    version: u32,
    configured_target: String,
    configuration: String,
    selection: String,
    crate_type: String,
    emit: String,
    dependency_link_strategy: String,
    declared_features: Vec<String>,
    source_tree: PathBuf,
    product_short_path: String,
    output_roles: Vec<String>,
    declared_inputs: BTreeMap<String, PathBuf>,
    input_roots: BTreeMap<String, PathBuf>,
    consumed_inputs: BTreeMap<String, PathBuf>,
    linked_inputs: BTreeMap<String, PathBuf>,
    control_inputs: BTreeMap<String, PathBuf>,
    roles: BTreeMap<String, Role>,
}

fn named(value: &str) -> Result<()> {
    require(
        !value.is_empty() && !value.contains('\0'),
        "empty or NUL action input identity",
    )
}
fn path(path: &Path) -> Result<()> {
    let spelling = path.to_str().ok_or("action input path is not UTF-8")?;
    named(spelling)
}
fn output_roles(selection: &str) -> Result<BTreeSet<&'static str>> {
    let mut roles = BTreeSet::from([
        "product",
        "directory",
        "invocation",
        "evidence",
        "finalization",
        "input_inventory",
    ]);
    match selection {
        "cdylib" => {
            roles.insert("qualified_product");
        }
        "code-rlib-static_pic" | "staticlib" => {}
        _ => return Err("action recipe selection is not ordinary codegen".into()),
    }
    Ok(roles)
}
fn merged_inputs(
    declared: &BTreeMap<String, PathBuf>,
    roots: &BTreeMap<String, PathBuf>,
    consumed: &BTreeMap<String, PathBuf>,
    linked: &BTreeMap<String, PathBuf>,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut merged = declared.clone();
    for (id, current) in declared
        .iter()
        .chain(roots.iter())
        .chain(consumed.iter())
        .chain(linked.iter())
    {
        named(id)?;
        path(current)?;
    }
    for (id, current) in roots.iter().chain(consumed.iter()).chain(linked.iter()) {
        if let Some(explicit) = id.strip_prefix("declared:") {
            require(
                declared.get(explicit) == Some(current),
                "explicit input alias loses its declared association",
            )?;
        }
        if let Some(previous) = merged.insert(id.clone(), current.clone()) {
            require(
                previous == *current,
                "logical input ID names different declared/current Artifacts",
            )?;
        }
    }
    Ok(merged)
}

fn validate_controls(controls: &BTreeMap<String, PathBuf>) -> Result<()> {
    require(
        controls.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == BTreeSet::from(["expected", "owner"]),
        "normal action controls must be exactly expected and owner",
    )?;
    for value in controls.values() {
        path(value)?;
    }
    Ok(())
}
fn read_recipe(bytes: &[u8]) -> Result<Recipe> {
    let value = super::native_wire::parse(bytes, MAX_RECIPE_BYTES)?;
    let recipe: Recipe =
        serde_json::from_value(value).map_err(|e| format!("normal action recipe: {e}"))?;
    require(
        recipe.version == 3,
        "unsupported normal action recipe version",
    )?;
    for value in [
        &recipe.configured_target,
        &recipe.configuration,
        &recipe.dependency_link_strategy,
        &recipe.product_short_path,
    ] {
        named(value)?;
    }
    path(&recipe.source_tree)?;
    require(
        match recipe.selection.as_str() {
            "cdylib" => recipe.crate_type == "cdylib" && recipe.emit == "link",
            "staticlib" => recipe.crate_type == "staticlib" && recipe.emit == "link",
            "code-rlib-static_pic" => recipe.crate_type == "rlib" && recipe.emit == "rlib",
            _ => false,
        },
        "normal action recipe is metadata, filtered or a different product contract",
    )?;
    let roles = output_roles(&recipe.selection)?;
    require(
        recipe.output_roles.len() == roles.len()
            && recipe
                .output_roles
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                == roles,
        "normal action recipe output role set differs",
    )?;
    require(
        recipe
            .declared_features
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            == recipe.declared_features.len(),
        "duplicate declared feature in action recipe",
    )?;
    merged_inputs(
        &recipe.declared_inputs,
        &recipe.input_roots,
        &recipe.consumed_inputs,
        &recipe.linked_inputs,
    )?;
    validate_controls(&recipe.control_inputs)?;
    validate_closure(&recipe.declared_inputs, &recipe.roles)?;
    Ok(recipe)
}

fn validate_role(role: &Role) -> Result<()> {
    named(&role.configured_target)?;
    named(&role.configuration)?;
    let mut keys = output_roles(&role.selection)?;
    keys.insert("recipe");
    // The actual normal rlib provider carries this companion. It may alias the
    // product on stable toolchains, but never acquires a qualification label.
    if role.selection == "code-rlib-static_pic" {
        keys.insert("metadata_full");
    }
    require(
        role.artifacts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == keys,
        "typed producer edge has missing or foreign output/metadata roles",
    )?;
    validate_output_paths(&role.artifacts)?;
    merged_inputs(
        &role.declared_inputs,
        &role.input_roots,
        &role.consumed_inputs,
        &role.linked_inputs,
    )?;
    validate_controls(&role.control_inputs)?;
    validate_closure(&role.declared_inputs, &role.roles)
}
fn validate_output_paths(artifacts: &BTreeMap<String, PathBuf>) -> Result<()> {
    for value in artifacts.values() {
        path(value)?;
    }
    let directory = artifacts
        .get("directory")
        .ok_or("producer directory absent")?;
    for (role, name) in [
        ("invocation", "invocation.json"),
        ("evidence", "evidence.json"),
        ("finalization", "finalization.json"),
        ("input_inventory", "input_inventory.json"),
    ] {
        require(
            artifacts.get(role) == Some(&directory.join(name)),
            "producer companion is outside its declared directory association",
        )?;
    }
    if let Some(qualified) = artifacts.get("qualified_product") {
        require(
            *qualified == directory.join("qualified/product"),
            "qualified product has a different declared owner association",
        )?;
    }
    Ok(())
}
fn validate_closure(
    declared: &BTreeMap<String, PathBuf>,
    roles: &BTreeMap<String, Role>,
) -> Result<()> {
    for (role, producer) in roles {
        named(role)?;
        validate_role(producer)?;
        for (kind, values) in [
            ("artifact", &producer.artifacts),
            ("declared", &producer.declared_inputs),
            ("root", &producer.input_roots),
            ("consumed", &producer.consumed_inputs),
            ("linked", &producer.linked_inputs),
            ("control", &producer.control_inputs),
        ] {
            for (id, current) in values {
                require(
                    declared.get(&format!("role/{role}/{kind}/{id}")) == Some(current),
                    format!("typed role {role} {kind} {id} is not the actual declared input"),
                )?;
            }
        }
    }
    Ok(())
}
#[derive(Debug, PartialEq, Eq)]
struct ConsumedProducts {
    code: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
}
fn consumed_products(
    selection: &str,
    consumed: &BTreeMap<String, PathBuf>,
    linked: &BTreeMap<String, PathBuf>,
    roles: &BTreeMap<String, Role>,
) -> Result<ConsumedProducts> {
    require(
        matches!(selection, "cdylib" | "staticlib") || linked.is_empty(),
        "nonlink code rlib has inherited linker inputs",
    )?;
    for (id, current) in linked {
        if let Some(direct) = consumed.get(id) {
            require(
                direct == current,
                "direct and linked input ID names different Artifacts",
            )?;
        }
    }
    let mut result = ConsumedProducts {
        code: BTreeMap::new(),
        metadata: BTreeMap::new(),
    };
    for (role, producer) in roles {
        let product = producer
            .artifacts
            .get("product")
            .ok_or("typed producer has no product")?;
        let metadata = producer.artifacts.get("metadata_full");
        let mut direct = false;
        let mut direct_code = false;
        for (id, current) in consumed {
            if current == product {
                direct = true;
                direct_code = true;
                bind_product(&mut result.code, id, role)?;
            }
            if metadata == Some(current) {
                direct = true;
                bind_product(&mut result.metadata, id, role)?;
            }
        }
        // The normal role association still requires a direct --extern input.
        // In advanced linking that input is metadata, not the code archive.
        require(
            direct,
            format!("typed role {role} has no actual code or metadata input"),
        )?;
        let mut linked_code = false;
        for (id, current) in linked {
            if current == product {
                linked_code = true;
                bind_product(&mut result.code, id, role)?;
            }
        }
        require(
            !matches!(selection, "cdylib" | "staticlib")
                || producer.selection != "code-rlib-static_pic"
                || direct_code
                || linked_code,
            format!("typed role {role} code is neither an actual compiler nor linker input"),
        )?;
    }
    for (id, role) in &result.metadata {
        if let Some(code_role) = result.code.get(id) {
            require(
                code_role == role,
                "one selected input claims different code and metadata roles",
            )?;
        }
    }
    Ok(result)
}

fn bind_product(bindings: &mut BTreeMap<String, String>, id: &str, role: &str) -> Result<()> {
    if let Some(previous) = bindings.insert(id.into(), role.into()) {
        require(
            previous == role,
            "one selected input claims different producer roles",
        )?;
    }
    Ok(())
}

fn relocation_join(
    original: &BTreeMap<String, PathBuf>,
    current: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    require(
        original.keys().eq(current.keys()),
        "relocated logical input sets differ",
    )?;
    let mut aliases = BTreeMap::new();
    for (id, old) in original {
        if let Some(previous) = aliases.insert(old, &current[id]) {
            require(
                previous == &current[id],
                "one original Artifact has inconsistent current alias mappings",
            )?;
        }
    }
    Ok(())
}

fn relocate_controls(
    original_files: &BTreeMap<String, PathBuf>,
    current_files: &BTreeMap<String, PathBuf>,
    original: &BTreeMap<String, PathBuf>,
    current: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    relocation_join(original_files, current_files)?;
    relocation_join(original, current)?;
    for (control, old) in original {
        for (id, file) in original_files {
            if old == file {
                require(
                    current[control] == current_files[id],
                    "control and semantic aliases have inconsistent current mappings",
                )?;
            }
        }
    }
    Ok(())
}

fn collect_roles(
    roles: &BTreeMap<String, Role>,
    collected: &mut BTreeMap<String, Role>,
) -> Result<()> {
    for (name, role) in roles {
        if let Some(previous) = collected.get(name) {
            require(
                previous == role,
                "repeated typed role has different producer/product/input associations",
            )?;
        } else {
            collected.insert(name.clone(), role.clone());
        }
        collect_roles(&role.roles, collected)?;
    }
    Ok(())
}

/// Enumerate only the exact recipe Artifacts supplied by observed typed edges.
/// The owner must read these paths with its normal file/parent guards; this
/// function performs no filesystem operation and grants no producer authority.
pub fn declared_recipe_paths(bytes: &[u8]) -> Result<BTreeSet<PathBuf>> {
    let recipe = read_recipe(bytes)?;
    let mut roles = BTreeMap::new();
    collect_roles(&recipe.roles, &mut roles)?;
    Ok(roles
        .values()
        .map(|role| role.artifacts["recipe"].clone())
        .collect())
}

/// Recover the complete original logical-ID-to-path map. The factory compares
/// it byte-for-byte with retained original inputs before using their compiler
/// spellings. This accessor neither opens nor resolves historical paths.
pub(super) fn original_input_paths(bytes: &[u8]) -> Result<BTreeMap<String, PathBuf>> {
    let recipe = read_recipe(bytes)?;
    require(
        recipe
            .input_roots
            .values()
            .any(|path| *path == recipe.source_tree),
        "original source tree lacks an actual action input",
    )?;
    merged_inputs(
        &recipe.declared_inputs,
        &recipe.input_roots,
        &recipe.consumed_inputs,
        &recipe.linked_inputs,
    )
}

fn same_role_structure(original: &Role, current: &Role) -> Result<()> {
    require(
        original.configured_target == current.configured_target
            && original.configuration == current.configuration
            && original.selection == current.selection
            && original.artifacts.keys().eq(current.artifacts.keys())
            && original
                .declared_inputs
                .keys()
                .eq(current.declared_inputs.keys())
            && original.input_roots.keys().eq(current.input_roots.keys())
            && original
                .consumed_inputs
                .keys()
                .eq(current.consumed_inputs.keys())
            && original
                .linked_inputs
                .keys()
                .eq(current.linked_inputs.keys())
            && original
                .control_inputs
                .keys()
                .eq(current.control_inputs.keys())
            && original.roles.keys().eq(current.roles.keys()),
        "retained recipe and current typed producer association differ",
    )?;
    relocate_controls(
        &merged_inputs(
            &original.declared_inputs,
            &original.input_roots,
            &original.consumed_inputs,
            &original.linked_inputs,
        )?,
        &merged_inputs(
            &current.declared_inputs,
            &current.input_roots,
            &current.consumed_inputs,
            &current.linked_inputs,
        )?,
        &original.control_inputs,
        &current.control_inputs,
    )?;
    require(
        consumed_products(
            &original.selection,
            &original.consumed_inputs,
            &original.linked_inputs,
            &original.roles,
        )? == consumed_products(
            &current.selection,
            &current.consumed_inputs,
            &current.linked_inputs,
            &current.roles,
        )?,
        "retained recipe and current typed consumed-product roles differ",
    )?;
    for (name, child) in &original.roles {
        same_role_structure(child, &current.roles[name])?;
    }
    Ok(())
}
fn current_source_tree(recipe: &Recipe, current: &Role) -> Result<PathBuf> {
    let mut mapped = BTreeSet::new();
    for (id, original) in &recipe.input_roots {
        if *original == recipe.source_tree {
            mapped.insert(
                current
                    .input_roots
                    .get(id)
                    .ok_or("source-tree logical input is missing")?
                    .clone(),
            );
        }
    }
    require(
        mapped.len() == 1,
        "recipe source tree lacks one exact current action-input association",
    )?;
    Ok(mapped.into_iter().next().unwrap())
}
fn producer(
    alias: &str,
    current: &Role,
    recipe: &Recipe,
    expected: &ExpectedProducer,
) -> Result<DeclaredProducer> {
    require(
        current.configured_target == recipe.configured_target
            && current.configuration == recipe.configuration
            && current.selection == recipe.selection
            && current
                .declared_inputs
                .keys()
                .eq(recipe.declared_inputs.keys())
            && current.input_roots.keys().eq(recipe.input_roots.keys())
            && current
                .consumed_inputs
                .keys()
                .eq(recipe.consumed_inputs.keys())
            && current.linked_inputs.keys().eq(recipe.linked_inputs.keys())
            && current
                .control_inputs
                .keys()
                .eq(recipe.control_inputs.keys())
            && current.roles.keys().eq(recipe.roles.keys()),
        "typed producer and its guarded recipe describe different actions",
    )?;
    for (name, original) in &recipe.roles {
        same_role_structure(original, &current.roles[name])?;
    }
    relocate_controls(
        &merged_inputs(
            &recipe.declared_inputs,
            &recipe.input_roots,
            &recipe.consumed_inputs,
            &recipe.linked_inputs,
        )?,
        &merged_inputs(
            &current.declared_inputs,
            &current.input_roots,
            &current.consumed_inputs,
            &current.linked_inputs,
        )?,
        &recipe.control_inputs,
        &current.control_inputs,
    )?;
    let dependencies = consumed_products(
        &current.selection,
        &current.consumed_inputs,
        &current.linked_inputs,
        &current.roles,
    )?;
    require(
        dependencies
            == consumed_products(
                &recipe.selection,
                &recipe.consumed_inputs,
                &recipe.linked_inputs,
                &recipe.roles,
            )?,
        "guarded recipe and current edge disagree about consumed code products",
    )?;
    let files = merged_inputs(
        &current.declared_inputs,
        &current.input_roots,
        &current.consumed_inputs,
        &current.linked_inputs,
    )?;
    require(
        expected.role == alias
            && expected.producer == current.configured_target
            && expected.configuration == current.configuration
            && expected.selection == current.selection
            && expected.features == recipe.declared_features
            && expected.files.keys().eq(files.keys())
            && expected.dependencies == dependencies.code
            && expected.metadata_dependencies == dependencies.metadata,
        "observed role/configuration/features/complete input or dependency set differs from independent expectations",
    )?;
    for (id, qualified) in current
        .consumed_inputs
        .keys()
        .map(|id| {
            (
                id,
                dependencies.code.contains_key(id) || dependencies.metadata.contains_key(id),
            )
        })
        .chain(
            current
                .linked_inputs
                .keys()
                .map(|id| (id, dependencies.code.contains_key(id))),
        )
    {
        if !qualified {
            let pin = &expected.files[id];
            require(
                pin.produced.is_none()
                    && pin.directory_members.is_none()
                    && pin.source_key.is_none()
                    && pin.compiler_filename.is_none()
                    && pin.sha256.len() == 64
                    && pin
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "unqualified selected input requires independently pinned regular bytes without source authority",
            )?;
        }
    }
    Ok(DeclaredProducer {
        role: alias.into(),
        producer: current.configured_target.clone(),
        configuration: current.configuration.clone(),
        selection: current.selection.clone(),
        files,
        declared_inputs: current.declared_inputs.clone(),
        input_roots: current.input_roots.clone(),
        consumed_inputs: current.consumed_inputs.clone(),
        linked_inputs: current.linked_inputs.clone(),
        control_inputs: current.control_inputs.clone(),
        source_tree: current_source_tree(recipe, current)?,
        recipe: current.artifacts["recipe"].clone(),
        product: current.artifacts["product"].clone(),
        directory: current.artifacts["directory"].clone(),
        invocation: current.artifacts["invocation"].clone(),
        evidence: current.artifacts["evidence"].clone(),
        finalization: current.artifacts["finalization"].clone(),
        input_inventory: current.artifacts["input_inventory"].clone(),
        qualified_product: current.artifacts.get("qualified_product").cloned(),
        metadata_full: current.artifacts.get("metadata_full").cloned(),
        dependencies: dependencies.code,
        metadata_dependencies: dependencies.metadata,
    })
}

/// Adapt guarded normal recipes and current typed-provider maps to plain graph
/// data. Dependency records must be read by the caller from exactly the paths
/// returned by `declared_recipe_paths`; extra ambient records are refused.
/// The root alias comes from the independent manifest only after the actual
/// configured producer/selection has matched. Dependency edges come entirely
/// from observed role maps and actual command inputs, never the expectation.
pub fn parse_declared_graph(
    bytes: &[u8],
    expected: &ExpectedGraph,
    outputs: &LeafOutputs,
    dependency_recipes: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<DeclaredGraph> {
    let recipe = read_recipe(bytes)?;
    let mut roles = BTreeMap::new();
    collect_roles(&recipe.roles, &mut roles)?;
    require(
        roles.keys().eq(expected.dependencies.keys()) && !roles.contains_key(&expected.leaf.role),
        "complete observed producer roles differ from independent graph",
    )?;
    let paths: BTreeSet<_> = roles
        .values()
        .map(|role| role.artifacts["recipe"].clone())
        .collect();
    require(
        paths == dependency_recipes.keys().cloned().collect(),
        "missing declared recipe or extra ambient recipe bytes",
    )?;
    let mut artifacts = BTreeMap::from([
        ("recipe".into(), outputs.recipe.clone()),
        ("product".into(), outputs.product.clone()),
        ("directory".into(), outputs.directory.clone()),
        ("invocation".into(), outputs.invocation.clone()),
        ("evidence".into(), outputs.evidence.clone()),
        ("finalization".into(), outputs.finalization.clone()),
        ("input_inventory".into(), outputs.input_inventory.clone()),
    ]);
    if let Some(qualified) = &outputs.qualified_product {
        artifacts.insert("qualified_product".into(), qualified.clone());
    }
    let mut wanted = output_roles(&recipe.selection)?;
    wanted.insert("recipe");
    require(
        artifacts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == wanted,
        "owner output CLI associations differ from the selected recipe",
    )?;
    validate_output_paths(&artifacts)?;
    let current = Role {
        configured_target: recipe.configured_target.clone(),
        configuration: recipe.configuration.clone(),
        selection: recipe.selection.clone(),
        artifacts,
        declared_inputs: recipe.declared_inputs.clone(),
        input_roots: recipe.input_roots.clone(),
        consumed_inputs: recipe.consumed_inputs.clone(),
        linked_inputs: recipe.linked_inputs.clone(),
        control_inputs: recipe.control_inputs.clone(),
        roles: recipe.roles.clone(),
    };
    let leaf = producer(&expected.leaf.role, &current, &recipe, &expected.leaf)?;
    let mut dependencies = BTreeMap::new();
    for (name, current) in &roles {
        let recipe = read_recipe(&dependency_recipes[&current.artifacts["recipe"]])?;
        dependencies.insert(
            name.clone(),
            producer(name, current, &recipe, &expected.dependencies[name])?,
        );
    }
    Ok(DeclaredGraph { leaf, dependencies })
}

fn retained_role(
    producer: &DeclaredProducer,
    dependencies: &BTreeMap<String, DeclaredProducer>,
    active: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<Role> {
    require(
        active.insert(producer.role.clone()),
        "current typed producer graph contains a dependency cycle",
    )?;
    require(
        producer.files
            == merged_inputs(
                &producer.declared_inputs,
                &producer.input_roots,
                &producer.consumed_inputs,
                &producer.linked_inputs,
            )?,
        "current producer loses declared/root/consumed/linked input associations",
    )?;
    validate_controls(&producer.control_inputs)?;
    let mut roles = BTreeMap::new();
    for name in producer
        .dependencies
        .values()
        .chain(producer.metadata_dependencies.values())
        .collect::<BTreeSet<_>>()
    {
        let child = dependencies
            .get(name)
            .ok_or("current consumed role has no typed producer")?;
        require(
            child.role == *name,
            "current dependency alias and producer role differ",
        )?;
        visited.insert(name.clone());
        roles.insert(
            name.clone(),
            retained_role(child, dependencies, active, visited)?,
        );
    }
    active.remove(&producer.role);
    let mut artifacts = BTreeMap::from([
        ("recipe".into(), producer.recipe.clone()),
        ("product".into(), producer.product.clone()),
        ("directory".into(), producer.directory.clone()),
        ("invocation".into(), producer.invocation.clone()),
        ("evidence".into(), producer.evidence.clone()),
        ("finalization".into(), producer.finalization.clone()),
        ("input_inventory".into(), producer.input_inventory.clone()),
    ]);
    if let Some(qualified) = &producer.qualified_product {
        artifacts.insert("qualified_product".into(), qualified.clone());
    }
    if let Some(metadata) = &producer.metadata_full {
        artifacts.insert("metadata_full".into(), metadata.clone());
    }
    Ok(Role {
        configured_target: producer.producer.clone(),
        configuration: producer.configuration.clone(),
        selection: producer.selection.clone(),
        artifacts,
        declared_inputs: producer.declared_inputs.clone(),
        input_roots: producer.input_roots.clone(),
        consumed_inputs: producer.consumed_inputs.clone(),
        linked_inputs: producer.linked_inputs.clone(),
        control_inputs: producer.control_inputs.clone(),
        roles,
    })
}

/// Recheck a retained producer's original recipes against the complete current
/// typed graph. The caller obtains dependency recipe bytes exclusively from
/// `current.dependencies` recipe paths, never from the retained leaf recipe's
/// historical path spellings. This is a data comparison, not record authority.
pub fn validate_retained_declared_graph(
    bytes: &[u8],
    expected: &ExpectedGraph,
    current: &DeclaredGraph,
    dependency_recipes: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<()> {
    require(
        current.dependencies.keys().eq(expected.dependencies.keys())
            && !current.dependencies.contains_key(&current.leaf.role),
        "retained current producer set differs from independent graph",
    )?;
    let paths: BTreeSet<_> = current
        .dependencies
        .values()
        .map(|producer| producer.recipe.clone())
        .collect();
    require(
        paths == dependency_recipes.keys().cloned().collect(),
        "retained graph lacks current recipe bytes or adds ambient recipes",
    )?;
    let mut visited = BTreeSet::new();
    let root = retained_role(
        &current.leaf,
        &current.dependencies,
        &mut BTreeSet::new(),
        &mut visited,
    )?;
    require(
        visited == current.dependencies.keys().cloned().collect(),
        "retained graph has an unconsumed producer",
    )?;
    // The root can be an rlib's own CLI edge, where metadata_full is absent.
    // Nested typed rlib roles must still carry their actual metadata companion.
    let mut root_keys = output_roles(&root.selection)?;
    root_keys.insert("recipe");
    if current.leaf.metadata_full.is_some() && root.selection == "code-rlib-static_pic" {
        root_keys.insert("metadata_full");
    }
    require(
        root.artifacts
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == root_keys,
        "retained owner output roles differ from its selected contract",
    )?;
    validate_output_paths(&root.artifacts)?;
    validate_closure(&root.declared_inputs, &root.roles)?;
    let recipe = read_recipe(bytes)?;
    require(
        producer(&current.leaf.role, &root, &recipe, &expected.leaf)? == current.leaf,
        "retained owner recipe does not rebind to the exact current graph",
    )?;
    let mut roles = BTreeMap::new();
    collect_roles(&root.roles, &mut roles)?;
    for (name, role) in roles {
        let recipe = read_recipe(&dependency_recipes[&role.artifacts["recipe"]])?;
        require(
            producer(&name, &role, &recipe, &expected.dependencies[&name])?
                == current.dependencies[&name],
            "retained dependency recipe does not rebind to its current typed producer",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::native_action::ExpectedFile;
    use super::super::native_action::Profile;
    use super::*;

    fn role(name: &str, selection: &str) -> Role {
        let base = PathBuf::from(format!("/current/{name}"));
        let directory = base.join("owner");
        let mut artifacts = BTreeMap::from([
            ("product".into(), base.join("product")),
            ("recipe".into(), base.join("recipe.json")),
            ("directory".into(), directory.clone()),
            ("invocation".into(), directory.join("invocation.json")),
            ("evidence".into(), directory.join("evidence.json")),
            ("finalization".into(), directory.join("finalization.json")),
            (
                "input_inventory".into(),
                directory.join("input_inventory.json"),
            ),
        ]);
        if selection == "cdylib" {
            artifacts.insert(
                "qualified_product".into(),
                directory.join("qualified/product"),
            );
        } else if selection == "code-rlib-static_pic" {
            artifacts.insert("metadata_full".into(), base.join("metadata"));
        }
        Role {
            configured_target: format!("root//:{name}"),
            configuration: "configured".into(),
            selection: selection.into(),
            artifacts,
            declared_inputs: BTreeMap::from([("source-tree".into(), base.join("src"))]),
            input_roots: BTreeMap::from([("owner:4:node:src".into(), base.join("src"))]),
            consumed_inputs: BTreeMap::new(),
            linked_inputs: BTreeMap::new(),
            control_inputs: BTreeMap::from([
                ("owner".into(), base.join("host")),
                ("expected".into(), base.join("expected.json")),
            ]),
            roles: BTreeMap::new(),
        }
    }
    fn attach(parent: &mut Role, name: &str, child: &Role, code: bool) {
        for (kind, map) in [
            ("artifact", &child.artifacts),
            ("declared", &child.declared_inputs),
            ("root", &child.input_roots),
            ("consumed", &child.consumed_inputs),
            ("linked", &child.linked_inputs),
            ("control", &child.control_inputs),
        ] {
            for (id, value) in map {
                parent
                    .declared_inputs
                    .insert(format!("role/{name}/{kind}/{id}"), value.clone());
            }
        }
        // A hidden code companion is present even for the metadata-only
        // intermediate compilation. Presence must not become consumption.
        parent.input_roots.insert(
            format!("hidden:{name}:code"),
            child.artifacts["product"].clone(),
        );
        if code {
            parent.consumed_inputs.insert(
                format!("owner:4:node:{name}-code"),
                child.artifacts["product"].clone(),
            );
            if parent.selection == "cdylib" {
                parent.linked_inputs.insert(
                    format!("owner:4:node:{name}-code"),
                    child.artifacts["product"].clone(),
                );
            }
        }
        parent.consumed_inputs.insert(
            format!("owner:4:node:{name}-metadata"),
            child.artifacts["metadata_full"].clone(),
        );
        parent.roles.insert(name.into(), child.clone());
    }
    fn recipe(role: &Role) -> Recipe {
        Recipe {
            version: 3,
            configured_target: role.configured_target.clone(),
            configuration: role.configuration.clone(),
            selection: role.selection.clone(),
            crate_type: match role.selection.as_str() {
                "cdylib" => "cdylib",
                "staticlib" => "staticlib",
                _ => "rlib",
            }
            .into(),
            emit: if matches!(role.selection.as_str(), "cdylib" | "staticlib") {
                "link"
            } else {
                "rlib"
            }
            .into(),
            dependency_link_strategy: "static_pic".into(),
            declared_features: Vec::new(),
            source_tree: role.input_roots["owner:4:node:src"].clone(),
            product_short_path: "product".into(),
            output_roles: output_roles(&role.selection)
                .unwrap()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            declared_inputs: role.declared_inputs.clone(),
            input_roots: role.input_roots.clone(),
            consumed_inputs: role.consumed_inputs.clone(),
            linked_inputs: role.linked_inputs.clone(),
            control_inputs: role.control_inputs.clone(),
            roles: role.roles.clone(),
        }
    }
    fn expected(
        name: &str,
        role: &Role,
        code: &[(&str, &str)],
        metadata: &[(&str, &str)],
    ) -> ExpectedProducer {
        // Independent synthetic requirements: product edges are passed explicitly,
        // rather than populated by the adapter's consumption calculation.
        let mut files = role.declared_inputs.clone();
        files.extend(role.input_roots.clone());
        files.extend(role.consumed_inputs.clone());
        files.extend(role.linked_inputs.clone());
        ExpectedProducer {
            external_linked_inputs: BTreeSet::new(),
            owner_pin: None,
            role: name.into(),
            producer: role.configured_target.clone(),
            configuration: role.configuration.clone(),
            selection: role.selection.clone(),
            crate_name: name.into(),
            features: Vec::new(),
            profile: Profile {
                opt_level: "3".into(),
                debuginfo: 0,
                debug_assertions: true,
                overflow_checks: true,
                test: false,
            },
            files: files
                .into_keys()
                .map(|key| {
                    (
                        key,
                        ExpectedFile {
                            produced: None,
                            directory_members: None,
                            sha256: "0".repeat(64),
                            size: 0,
                            source_key: None,
                            compiler_filename: None,
                        },
                    )
                })
                .collect(),
            wrapper_argv: Vec::new(),
            wrapper_environment: BTreeMap::new(),
            wrapper_environment_contract: None,
            compiler_argv: Vec::new(),
            compiler_environment: BTreeMap::new(),
            cwd: Vec::new(),
            dependencies: code
                .iter()
                .map(|(id, target)| ((*id).into(), (*target).into()))
                .collect(),
            metadata_dependencies: metadata
                .iter()
                .map(|(id, target)| ((*id).into(), (*target).into()))
                .collect(),
        }
    }
    fn outputs(role: &Role) -> LeafOutputs {
        LeafOutputs {
            recipe: role.artifacts["recipe"].clone(),
            product: role.artifacts["product"].clone(),
            directory: role.artifacts["directory"].clone(),
            invocation: role.artifacts["invocation"].clone(),
            evidence: role.artifacts["evidence"].clone(),
            finalization: role.artifacts["finalization"].clone(),
            input_inventory: role.artifacts["input_inventory"].clone(),
            qualified_product: role.artifacts.get("qualified_product").cloned(),
        }
    }
    struct Fixture {
        root: Role,
        expected: ExpectedGraph,
        recipes: BTreeMap<PathBuf, Vec<u8>>,
    }
    fn fixture() -> Fixture {
        let guest = role("guest", "code-rlib-static_pic");
        let mut middle = role("middle", "code-rlib-static_pic");
        attach(&mut middle, "guest", &guest, false);
        let mut root = role("root", "cdylib");
        attach(&mut root, "guest", &guest, true);
        attach(&mut root, "middle", &middle, true);
        let expected = ExpectedGraph {
            leaf: expected(
                "root",
                &root,
                &[
                    ("owner:4:node:guest-code", "guest"),
                    ("owner:4:node:middle-code", "middle"),
                ],
                &[
                    ("owner:4:node:guest-metadata", "guest"),
                    ("owner:4:node:middle-metadata", "middle"),
                ],
            ),
            dependencies: BTreeMap::from([
                ("guest".into(), expected("guest", &guest, &[], &[])),
                (
                    "middle".into(),
                    expected(
                        "middle",
                        &middle,
                        &[],
                        &[("owner:4:node:guest-metadata", "guest")],
                    ),
                ),
            ]),
        };
        let recipes = [&guest, &middle]
            .into_iter()
            .map(|producer| {
                (
                    producer.artifacts["recipe"].clone(),
                    serde_json::to_vec(&recipe(producer)).unwrap(),
                )
            })
            .collect();
        Fixture {
            root,
            expected,
            recipes,
        }
    }
    fn parse(fixture: &Fixture) -> Result<DeclaredGraph> {
        parse_declared_graph(
            &serde_json::to_vec(&recipe(&fixture.root)).unwrap(),
            &fixture.expected,
            &outputs(&fixture.root),
            &fixture.recipes,
        )
    }

    #[test]
    fn normal_nested_metadata_compile_and_code_link_remain_distinct() {
        let mut fixture = fixture();
        let graph = parse(&fixture).unwrap();
        assert_eq!(graph.leaf.dependencies.len(), 2);
        assert!(graph.dependencies["middle"].dependencies.is_empty());
        assert_eq!(
            graph.dependencies["middle"].files["hidden:guest:code"],
            graph.dependencies["guest"].product
        );
        assert!(
            !graph.dependencies["middle"]
                .consumed_inputs
                .contains_key("hidden:guest:code")
        );
        assert_eq!(
            graph.dependencies["middle"].metadata_dependencies["owner:4:node:guest-metadata"],
            "guest"
        );
        assert!(graph.dependencies["middle"].qualified_product.is_none());
        assert_eq!(
            graph.dependencies["guest"].metadata_full.as_ref(),
            Some(&PathBuf::from("/current/guest/metadata"))
        );
        fixture
            .expected
            .dependencies
            .get_mut("middle")
            .unwrap()
            .dependencies
            .insert("owner:4:node:guest-metadata".into(), "guest".into());
        assert!(
            parse(&fixture).is_err(),
            "expectation relabelled metadata as code"
        );
    }

    #[test]
    fn cache_recipe_paths_rebind_only_through_exact_logical_inputs() {
        let mut fixture = fixture();
        for bytes in fixture.recipes.values_mut() {
            *bytes = String::from_utf8(bytes.clone())
                .unwrap()
                .replace("/current/", "/original-cache/")
                .into_bytes();
        }
        let graph = parse(&fixture).unwrap();
        assert_eq!(
            graph.dependencies["guest"].source_tree,
            PathBuf::from("/current/guest/src")
        );
        assert_eq!(
            graph.dependencies["middle"].source_tree,
            PathBuf::from("/current/middle/src")
        );
        assert!(
            fixture
                .recipes
                .values()
                .all(|bytes| String::from_utf8_lossy(bytes).contains("/original-cache/"))
        );
        let path = fixture.root.roles["guest"].artifacts["recipe"].clone();
        let mut record: serde_json::Value =
            serde_json::from_slice(&fixture.recipes[&path]).unwrap();
        record["source_tree"] = serde_json::json!("/unmapped/same-basename/src");
        fixture
            .recipes
            .insert(path, serde_json::to_vec(&record).unwrap());
        assert!(
            parse(&fixture).is_err(),
            "source tree was guessed from a suffix"
        );
    }

    #[test]
    fn swapped_companions_missing_declared_edges_and_ambient_recipes_refuse() {
        let mut fixture = fixture();
        fixture.root.declared_inputs.insert(
            "role/guest/artifact/evidence".into(),
            PathBuf::from("/foreign/evidence.json"),
        );
        assert!(parse(&fixture).is_err());
        let mut fixture = self::fixture();
        fixture.root.consumed_inputs.insert(
            "owner:4:node:guest-code".into(),
            fixture.root.roles["guest"].artifacts["metadata_full"].clone(),
        );
        assert!(
            parse(&fixture).is_err(),
            "metadata replaced the consumed code archive"
        );
        let mut fixture = self::fixture();
        fixture
            .recipes
            .insert(PathBuf::from("/ambient/recipe.json"), b"{}".to_vec());
        assert!(parse(&fixture).is_err());
        let fixture = self::fixture();
        let bytes = serde_json::to_vec(&recipe(&fixture.root)).unwrap();
        assert_eq!(
            declared_recipe_paths(&bytes).unwrap(),
            fixture.recipes.keys().cloned().collect()
        );
        assert!(declared_recipe_paths(br#"{"version":2,"version":2}"#).is_err());
    }

    #[test]
    fn explicit_ownerless_aliases_survive_and_conflicting_ids_refuse() {
        let declared = BTreeMap::from([
            ("one".into(), PathBuf::from("/current/file")),
            ("two".into(), PathBuf::from("/current/file")),
        ]);
        let action = BTreeMap::from([
            ("declared:one".into(), PathBuf::from("/current/file")),
            ("declared:two".into(), PathBuf::from("/current/file")),
        ]);
        let merged = merged_inputs(&declared, &action, &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(merged.len(), 4);
        let mut missing = declared.clone();
        missing.remove("two");
        assert!(merged_inputs(&missing, &action, &BTreeMap::new(), &BTreeMap::new()).is_err());
        let mut collision = declared.clone();
        collision.insert("declared:one".into(), PathBuf::from("/different/file"));
        assert!(merged_inputs(&collision, &action, &BTreeMap::new(), &BTreeMap::new()).is_err());
        let mut moved = merged.clone();
        moved.insert("two".into(), PathBuf::from("/different/file"));
        assert!(relocation_join(&merged, &moved).is_err());
    }

    #[test]
    fn retained_leaf_rebinds_without_discovering_original_recipe_paths() {
        let fixture = fixture();
        let graph = parse(&fixture).unwrap();
        let historical = |bytes: &[u8]| {
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .replace("/current/", "/original-cache/")
                .into_bytes()
        };
        let bytes = historical(&serde_json::to_vec(&recipe(&fixture.root)).unwrap());
        let original = original_input_paths(&bytes).unwrap();
        assert_eq!(
            original["owner:4:node:src"],
            PathBuf::from("/original-cache/root/src")
        );
        assert_eq!(original.len(), graph.leaf.files.len());
        let recipes = fixture
            .recipes
            .iter()
            .map(|(path, bytes)| (path.clone(), historical(bytes)))
            .collect();
        validate_retained_declared_graph(&bytes, &fixture.expected, &graph, &recipes).unwrap();
        assert!(
            declared_recipe_paths(&bytes)
                .unwrap()
                .iter()
                .all(|path| { path.starts_with("/original-cache") && !recipes.contains_key(path) })
        );

        let mut wrong = graph.clone();
        wrong.leaf.source_tree = PathBuf::from("/unrelated/current/src");
        assert!(
            validate_retained_declared_graph(&bytes, &fixture.expected, &wrong, &recipes).is_err()
        );
        let mut wrong = graph.clone();
        wrong
            .dependencies
            .get_mut("middle")
            .unwrap()
            .metadata_dependencies
            .clear();
        assert!(
            validate_retained_declared_graph(&bytes, &fixture.expected, &wrong, &recipes).is_err()
        );
        let mut wrong = graph.clone();
        wrong
            .dependencies
            .get_mut("guest")
            .unwrap()
            .dependencies
            .insert("cycle".into(), "middle".into());
        assert!(
            validate_retained_declared_graph(&bytes, &fixture.expected, &wrong, &recipes).is_err()
        );
        let stale_recipes = recipes
            .into_iter()
            .map(|(path, bytes)| {
                (
                    PathBuf::from(
                        path.to_str()
                            .unwrap()
                            .replace("/current/", "/original-cache/"),
                    ),
                    bytes,
                )
            })
            .collect();
        assert!(
            validate_retained_declared_graph(&bytes, &fixture.expected, &graph, &stale_recipes)
                .is_err()
        );
    }

    #[test]
    fn version_two_legacy_fields_and_unbound_controls_inventory_refuse() {
        let fixture = fixture();
        let mut old = recipe(&fixture.root);
        old.version = 2;
        assert!(declared_recipe_paths(&serde_json::to_vec(&old).unwrap()).is_err());
        let mut value = serde_json::to_value(recipe(&fixture.root)).unwrap();
        value["action_inputs"] = serde_json::json!({});
        assert!(declared_recipe_paths(&serde_json::to_vec(&value).unwrap()).is_err());
        for key in ["owner", "expected"] {
            let mut missing = recipe(&fixture.root);
            missing.control_inputs.remove(key);
            assert!(declared_recipe_paths(&serde_json::to_vec(&missing).unwrap()).is_err());
        }
        let mut extra = recipe(&fixture.root);
        extra
            .control_inputs
            .insert("ambient".into(), PathBuf::from("/ambient"));
        assert!(declared_recipe_paths(&serde_json::to_vec(&extra).unwrap()).is_err());
        let mut wrong_outputs = outputs(&fixture.root);
        wrong_outputs.input_inventory = PathBuf::from("/foreign/input_inventory.json");
        assert!(
            parse_declared_graph(
                &serde_json::to_vec(&recipe(&fixture.root)).unwrap(),
                &fixture.expected,
                &wrong_outputs,
                &fixture.recipes
            )
            .is_err()
        );
        let mut legacy_namespace = recipe(&fixture.root);
        let path = legacy_namespace
            .declared_inputs
            .remove("role/guest/root/owner:4:node:src")
            .unwrap();
        legacy_namespace
            .declared_inputs
            .insert("role/guest/action/owner:4:node:src".into(), path);
        assert!(declared_recipe_paths(&serde_json::to_vec(&legacy_namespace).unwrap()).is_err());
    }

    #[test]
    fn hidden_code_presence_cannot_replace_selected_metadata_consumption() {
        let fixture = fixture();
        let middle = &fixture.root.roles["middle"];
        let observed = consumed_products(
            &middle.selection,
            &middle.consumed_inputs,
            &middle.linked_inputs,
            &middle.roles,
        )
        .unwrap();
        assert!(observed.code.is_empty());
        assert_eq!(
            observed
                .metadata
                .values()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["guest"]
        );
        assert!(
            middle
                .input_roots
                .values()
                .any(|path| *path == middle.roles["guest"].artifacts["product"])
        );
        let mut missing_pin = recipe(&fixture.root);
        missing_pin.consumed_inputs.insert(
            "unlisted-extern".into(),
            PathBuf::from("/third-party/library.rlib"),
        );
        assert!(
            parse_declared_graph(
                &serde_json::to_vec(&missing_pin).unwrap(),
                &fixture.expected,
                &outputs(&fixture.root),
                &fixture.recipes
            )
            .is_err()
        );
        let mut wrong_expected = fixture.expected.clone();
        wrong_expected
            .dependencies
            .get_mut("middle")
            .unwrap()
            .dependencies
            .insert("hidden:guest:code".into(), "guest".into());
        assert!(
            parse_declared_graph(
                &serde_json::to_vec(&recipe(&fixture.root)).unwrap(),
                &wrong_expected,
                &outputs(&fixture.root),
                &fixture.recipes
            )
            .is_err()
        );
    }
    #[test]
    fn unqualified_extern_keeps_independent_pin_without_joining_the_qualified_cohort() {
        let mut fixture = fixture();
        let id = "external:third-party".to_owned();
        let path = PathBuf::from("/third-party/library.rlib");
        fixture.root.consumed_inputs.insert(id.clone(), path);
        let pin = ExpectedFile {
            produced: None,
            directory_members: None,
            sha256: "a".repeat(64),
            size: 123,
            source_key: None,
            compiler_filename: None,
        };
        fixture.expected.leaf.files.insert(id.clone(), pin);
        let graph = parse(&fixture).unwrap();
        assert_eq!(graph.leaf.files.len(), fixture.expected.leaf.files.len());
        assert!(!graph.leaf.dependencies.contains_key(&id));
        assert!(!graph.leaf.metadata_dependencies.contains_key(&id));
        assert_eq!(graph.dependencies.len(), 2);
        for change in 0..4 {
            let mut wrong = fixture.expected.clone();
            let pin = wrong.leaf.files.get_mut(&id).unwrap();
            match change {
                0 => pin.sha256.clear(),
                1 => pin.source_key = Some("rv:unqualified.rs".into()),
                2 => pin.directory_members = Some(BTreeMap::new()),
                _ => {
                    pin.produced = Some(super::super::native_action::ProducedArtifact {
                        role: "guest".into(),
                        output: super::super::native_action::ProducerOutput::Product,
                    })
                }
            }
            assert!(
                parse_declared_graph(
                    &serde_json::to_vec(&recipe(&fixture.root)).unwrap(),
                    &wrong,
                    &outputs(&fixture.root),
                    &fixture.recipes
                )
                .is_err()
            );
        }
    }
    // Synthetic normal-wire controls only. These compare associations and do
    // not execute rustc or grant native compilation/source authority.
    #[test]
    fn stable_and_advanced_linking_preserve_distinct_selected_inputs() {
        let both = fixture();
        let both_graph = parse(&both).unwrap();
        assert_eq!(both_graph.leaf.dependencies.len(), 2);
        assert_eq!(both_graph.leaf.linked_inputs.len(), 2);
        assert_eq!(both_graph.leaf.metadata_dependencies.len(), 2);

        let mut stable = fixture();
        stable.root.linked_inputs.clear();
        let stable_graph = parse(&stable).unwrap();
        assert_eq!(stable_graph.leaf.dependencies, both_graph.leaf.dependencies);
        assert!(stable_graph.leaf.linked_inputs.is_empty());

        let mut advanced = fixture();
        for id in ["owner:4:node:guest-code", "owner:4:node:middle-code"] {
            advanced.root.consumed_inputs.remove(id).unwrap();
        }
        let advanced_graph = parse(&advanced).unwrap();
        assert_eq!(
            advanced_graph.leaf.dependencies,
            both_graph.leaf.dependencies
        );
        assert_eq!(
            advanced_graph.leaf.metadata_dependencies,
            both_graph.leaf.metadata_dependencies
        );
        assert!(advanced_graph.leaf.dependencies.keys().all(|id| {
            !advanced_graph.leaf.consumed_inputs.contains_key(id)
                && advanced_graph.leaf.linked_inputs.contains_key(id)
        }));

        advanced
            .root
            .linked_inputs
            .remove("owner:4:node:guest-code");
        let error = parse(&advanced).unwrap_err();
        assert!(
            error.contains("neither an actual compiler nor linker input"),
            "{error}"
        );
        assert!(advanced.root.input_roots.contains_key("hidden:guest:code"));
        assert!(
            advanced
                .root
                .consumed_inputs
                .contains_key("owner:4:node:guest-metadata")
        );
    }

    #[test]
    fn linked_map_is_required_and_does_not_replace_direct_metadata() {
        let fixture = fixture();
        for nested in [false, true] {
            let mut value = serde_json::to_value(recipe(&fixture.root)).unwrap();
            let object = if nested {
                &mut value["roles"]["guest"]
            } else {
                &mut value
            };
            object.as_object_mut().unwrap().remove("linked_inputs");
            assert!(declared_recipe_paths(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let mut unknown = serde_json::to_value(recipe(&fixture.root)).unwrap();
        unknown["linked_files"] = serde_json::json!({});
        assert!(declared_recipe_paths(&serde_json::to_vec(&unknown).unwrap()).is_err());

        let mut absent_direct = self::fixture();
        absent_direct
            .root
            .consumed_inputs
            .remove("owner:4:node:guest-code");
        absent_direct
            .root
            .consumed_inputs
            .remove("owner:4:node:guest-metadata");
        let error = parse(&absent_direct).unwrap_err();
        assert!(
            error.contains("no actual code or metadata input"),
            "{error}"
        );
        assert!(
            absent_direct
                .root
                .linked_inputs
                .contains_key("owner:4:node:guest-code")
        );
    }

    #[test]
    fn linked_ids_cannot_change_artifact_or_claim_another_role() {
        let mut fixture = fixture();
        fixture.root.linked_inputs.insert(
            "owner:4:node:guest-code".into(),
            fixture.root.roles["guest"].artifacts["metadata_full"].clone(),
        );
        assert!(
            parse(&fixture)
                .unwrap_err()
                .contains("different declared/current Artifacts")
        );

        let mut fixture = self::fixture();
        let shared = fixture.root.roles["guest"].artifacts["product"].clone();
        fixture
            .root
            .roles
            .get_mut("middle")
            .unwrap()
            .artifacts
            .insert("product".into(), shared);
        let error = consumed_products(
            &fixture.root.selection,
            &fixture.root.consumed_inputs,
            &fixture.root.linked_inputs,
            &fixture.root.roles,
        )
        .unwrap_err();
        assert!(error.contains("different producer roles"), "{error}");
    }

    #[test]
    fn unqualified_linkables_require_pins_without_gaining_source_or_role_authority() {
        let mut fixture = fixture();
        // The ID is opaque to Reverie. A caller may use its own qualified source
        // identity, but no parsing of cells, packages or basenames occurs here.
        let id = "source:caller-cell//opaque/libnative.a".to_owned();
        let current = PathBuf::from("/materialized/native/selected.a");
        fixture
            .root
            .linked_inputs
            .insert(id.clone(), current.clone());
        let pin = ExpectedFile {
            produced: None,
            directory_members: None,
            sha256: "b".repeat(64),
            size: 456,
            source_key: None,
            compiler_filename: None,
        };
        fixture.expected.leaf.files.insert(id.clone(), pin);
        let graph = parse(&fixture).unwrap();
        assert_eq!(graph.leaf.linked_inputs[&id], current);
        assert_eq!(graph.leaf.files[&id], current);
        assert!(!graph.leaf.dependencies.contains_key(&id));
        assert!(!graph.leaf.metadata_dependencies.contains_key(&id));
        assert_eq!(graph.dependencies.len(), 2);
        super::super::native_action::declared_link_coverage(
            &fixture.expected.leaf,
            &graph.leaf,
            &graph,
        )
        .unwrap();
        for change in 0..5 {
            let mut wrong = fixture.expected.clone();
            let pin = wrong.leaf.files.get_mut(&id).unwrap();
            match change {
                0 => pin.sha256.clear(),
                1 => pin.source_key = Some("rv:not-qualified.rs".into()),
                2 => pin.directory_members = Some(BTreeMap::new()),
                3 => pin.compiler_filename = Some(Vec::new()),
                _ => {
                    pin.produced = Some(super::super::native_action::ProducedArtifact {
                        role: "guest".into(),
                        output: super::super::native_action::ProducerOutput::Product,
                    })
                }
            }
            assert!(
                parse_declared_graph(
                    &serde_json::to_vec(&recipe(&fixture.root)).unwrap(),
                    &wrong,
                    &outputs(&fixture.root),
                    &fixture.recipes,
                )
                .is_err()
            );
            assert!(
                super::super::native_action::declared_link_coverage(
                    &wrong.leaf,
                    &graph.leaf,
                    &graph
                )
                .is_err()
            );
        }
        fixture.expected.leaf.files.remove(&id);
        assert!(parse(&fixture).is_err());
    }

    #[test]
    fn typed_factory_requires_selected_code_even_without_json_adaptation() {
        let fixture = fixture();
        let graph = parse(&fixture).unwrap();
        let check = |graph: &DeclaredGraph| {
            super::super::native_action::declared_link_coverage(
                &fixture.expected.leaf,
                &graph.leaf,
                graph,
            )
        };
        check(&graph).unwrap();
        let mut stable = graph.clone();
        stable.leaf.linked_inputs.clear();
        check(&stable).unwrap();
        let mut advanced = graph.clone();
        for id in ["owner:4:node:guest-code", "owner:4:node:middle-code"] {
            advanced.leaf.consumed_inputs.remove(id).unwrap();
        }
        check(&advanced).unwrap();
        let mut no_direct = advanced.clone();
        no_direct
            .leaf
            .consumed_inputs
            .remove("owner:4:node:guest-metadata");
        assert!(
            check(&no_direct).is_err(),
            "linked code replaced the required direct compiler association"
        );
        advanced.leaf.linked_inputs.clear();
        assert!(
            check(&advanced).is_err(),
            "hidden/inventory code presence replaced actual selection"
        );
        let mut wrong = graph.clone();
        wrong.leaf.linked_inputs.insert(
            "owner:4:node:guest-code".into(),
            graph.dependencies["middle"].product.clone(),
        );
        assert!(check(&wrong).is_err());
        let mut wrong = graph.clone();
        wrong.leaf.linked_inputs.insert(
            "unlisted-native".into(),
            PathBuf::from("/not-expected/lib.a"),
        );
        assert!(check(&wrong).is_err());
        let mut nonlink = graph.dependencies["middle"].clone();
        nonlink.linked_inputs.insert(
            "hidden:guest:code".into(),
            graph.dependencies["guest"].product.clone(),
        );
        assert!(
            super::super::native_action::declared_link_coverage(
                &fixture.expected.dependencies["middle"],
                &nonlink,
                &graph,
            )
            .is_err()
        );
        let middle = &fixture.root.roles["middle"];
        assert!(
            consumed_products(
                &middle.selection,
                &middle.consumed_inputs,
                &nonlink.linked_inputs,
                &middle.roles,
            )
            .is_err()
        );
    }

    #[test]
    fn retained_linked_inputs_rebind_by_exact_ids_and_keep_original_aliases() {
        let mut fixture = fixture();
        for id in ["owner:4:node:guest-code", "owner:4:node:middle-code"] {
            fixture.root.consumed_inputs.remove(id).unwrap();
        }
        let graph = parse(&fixture).unwrap();
        let historical = |bytes: &[u8]| {
            String::from_utf8(bytes.to_vec())
                .unwrap()
                .replace("/current/", "/original-cache/")
                .into_bytes()
        };
        let bytes = historical(&serde_json::to_vec(&recipe(&fixture.root)).unwrap());
        let recipes = fixture
            .recipes
            .iter()
            .map(|(path, bytes)| (path.clone(), historical(bytes)))
            .collect();
        validate_retained_declared_graph(&bytes, &fixture.expected, &graph, &recipes).unwrap();
        let original = original_input_paths(&bytes).unwrap();
        assert_eq!(
            original["owner:4:node:guest-code"],
            PathBuf::from("/original-cache/guest/product")
        );
        assert_eq!(
            graph.leaf.linked_inputs["owner:4:node:guest-code"],
            PathBuf::from("/current/guest/product")
        );
        let mut wrong = graph.clone();
        wrong.leaf.linked_inputs.remove("owner:4:node:guest-code");
        assert!(
            validate_retained_declared_graph(&bytes, &fixture.expected, &wrong, &recipes).is_err()
        );
        let mut wrong = graph.clone();
        wrong.leaf.linked_inputs.insert(
            "owner:4:node:guest-code".into(),
            PathBuf::from("/unrelated/product"),
        );
        wrong.leaf.files.insert(
            "owner:4:node:guest-code".into(),
            PathBuf::from("/unrelated/product"),
        );
        assert!(
            validate_retained_declared_graph(&bytes, &fixture.expected, &wrong, &recipes).is_err()
        );
    }

    #[test]
    fn staticlib_recipe_retains_code_edges_without_a_dso_qualified_output() {
        let mut fixture = fixture();
        fixture.root.selection = "staticlib".into();
        fixture.root.artifacts.remove("qualified_product");
        fixture.expected.leaf.selection = "staticlib".into();
        let graph = parse(&fixture).unwrap();
        assert_eq!(graph.leaf.selection, "staticlib");
        assert!(graph.leaf.qualified_product.is_none());
        assert!(graph.leaf.metadata_full.is_none());
        assert_eq!(graph.leaf.dependencies.len(), 2);
        let valid = recipe(&fixture.root);
        for field in ["crate_type", "emit", "output_roles"] {
            let mut wrong = serde_json::to_value(&valid).unwrap();
            wrong[field] = match field {
                "crate_type" => serde_json::json!("rlib"),
                "emit" => serde_json::json!("metadata"),
                _ => serde_json::json!([
                    "product",
                    "directory",
                    "invocation",
                    "evidence",
                    "finalization",
                    "input_inventory",
                    "qualified_product"
                ]),
            };
            assert!(
                read_recipe(&serde_json::to_vec(&wrong).unwrap()).is_err(),
                "{field}"
            );
        }
        // Stable linking consumes code directly; removing only inherited
        // link inputs must preserve that independently selected code edge.
        let linked = fixture.root.linked_inputs.clone();
        fixture.root.linked_inputs.clear();
        assert!(parse(&fixture).is_ok());
        fixture.root.linked_inputs = linked;
        // Advanced linking has metadata externs and separately linked code.
        for id in ["owner:4:node:guest-code", "owner:4:node:middle-code"] {
            fixture.root.consumed_inputs.remove(id).unwrap();
        }
        assert!(parse(&fixture).is_ok());
        // Complete inventory presence and a metadata extern cannot replace the
        // actual selected code edge of the staticlib-producing action.
        fixture.root.linked_inputs.clear();
        assert!(parse(&fixture).is_err());
    }
}
