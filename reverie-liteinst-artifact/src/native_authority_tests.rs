// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

// Included in lib.rs's test module. These are structural boundary controls:
// fixture records never construct source authority or qualify a native build.
fn native_authority_physical_boundary_controls(
    backing: &AllocatorBackingEvidence,
    proof: &NoInstallationScopeEvidence,
) {
    let cohort = validate_cargo_no_scope_binding(proof).unwrap();
    assert_eq!(cohort, NoScopeCohort::DetcoreNative101);
    let physical = proof.physical();
    let allocator = AllocatorBackingPhysical::from_cargo(backing);
    validate_no_scope_physical(&allocator, &physical, cohort, FixedImageKind::Detcore).unwrap();

    let packed = NoInstallationScopeEvidence::from_physical(
        physical.clone(),
        proof.compiled_sources_sha256.clone(),
        proof.reverie_pin.clone(),
        proof.current_build.clone(),
    );
    assert_eq!(&packed, proof, "physical packing lost a historical field");
    assert_eq!(
        serde_json::to_vec(&packed).unwrap(),
        serde_json::to_vec(proof).unwrap(),
        "physical packing changed the historical Cargo envelope"
    );
    let packed_allocator = allocator.clone().into_cargo(backing.tls_boundary.clone());
    assert_eq!(&packed_allocator, backing);
    assert_eq!(
        serde_json::to_vec(&packed_allocator).unwrap(),
        serde_json::to_vec(backing).unwrap(),
        "allocator packing changed existing installation evidence"
    );
    let boundary = AllocatorInstallationEvidence::NoInstallationScope(Box::new(packed));
    let mut expected_allocator = backing.clone();
    expected_allocator.tls_boundary = boundary.clone();
    let no_scope_allocator = allocator.clone().into_cargo(boundary);
    assert_eq!(no_scope_allocator, expected_allocator);
    let historical_bytes = serde_json::to_vec(&expected_allocator).unwrap();
    assert_eq!(
        serde_json::to_vec(&no_scope_allocator).unwrap(),
        historical_bytes
    );
    assert_eq!(
        serde_json::from_slice::<AllocatorBackingEvidence>(&historical_bytes).unwrap(),
        expected_allocator
    );

    // A successful physical check must not launder unrelated or failed build
    // evidence. Each mutation preserves exactly the same physical payload.
    let reject_binding = |label: &str, changed: NoInstallationScopeEvidence| {
        assert_eq!(
            changed.physical(),
            physical,
            "{label} changed physical proof"
        );
        validate_no_scope_physical(
            &allocator,
            &changed.physical(),
            cohort,
            FixedImageKind::Detcore,
        )
        .unwrap();
        assert!(
            validate_cargo_no_scope_binding(&changed).is_err(),
            "physical payload admitted {label} at Cargo binding"
        );
        assert!(
            validate_no_installation_scope_evidence(backing, &changed, FixedImageKind::Detcore)
                .is_err(),
            "historical validator omitted {label} binding"
        );
    };
    let mut failed_build = proof.clone();
    failed_build.current_build.command.status = 1;
    reject_binding("failed build with retained output", failed_build);
    let mut wrong_sources = proof.clone();
    wrong_sources.current_build.compiled_sources_sha256 = "0".repeat(64);
    reject_binding("different compiled sources", wrong_sources);
    let mut wrong_product = proof.clone();
    wrong_product.current_build.artifact.sha256 = "0".repeat(64);
    reject_binding("different selected product", wrong_product);
    let mut wrong_dependency = proof.clone();
    wrong_dependency
        .current_build
        .packages
        .get_mut("reverie-inguest")
        .unwrap()
        .profile["debug_assertions"] = serde_json::json!(false);
    reject_binding(
        "different dependency profile with unchanged leaf",
        wrong_dependency,
    );

    // The ordinary cohort is valid on its own. It cannot authorize this native
    // cohort's unchanged TLS population, even though source/output joins agree.
    let mut other_cohort = proof.clone();
    for (name, package) in &mut other_cohort.current_build.packages {
        package.features = if name == "reverie-inguest" {
            ["coordinator-rpc", "launcher", "rcb-clock"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        } else {
            Vec::new()
        };
    }
    let other = validate_cargo_no_scope_binding(&other_cohort).unwrap();
    assert_eq!(other, NoScopeCohort::DetcoreOrdinary101);
    assert_eq!(other_cohort.physical(), physical);
    assert!(
        validate_no_scope_physical(&allocator, &physical, other, FixedImageKind::Detcore).is_err(),
        "a different valid cohort authorized the unchanged physical payload"
    );
    assert!(
        validate_no_installation_scope_evidence(backing, &other_cohort, FixedImageKind::Detcore)
            .is_err()
    );

    let reject_physical = |label: &str, changed: NoScopePhysical| {
        assert!(
            validate_no_scope_physical(&allocator, &changed, cohort, FixedImageKind::Detcore)
                .is_err(),
            "shared physical validator admitted {label}"
        );
        let repacked = NoInstallationScopeEvidence::from_physical(
            changed,
            proof.compiled_sources_sha256.clone(),
            proof.reverie_pin.clone(),
            proof.current_build.clone(),
        );
        assert_eq!(validate_cargo_no_scope_binding(&repacked).unwrap(), cohort);
        assert!(
            validate_no_installation_scope_evidence(backing, &repacked, FixedImageKind::Detcore)
                .is_err(),
            "Cargo packing bypassed the refusal for {label}"
        );
    };
    let mut wrong_source = physical.clone();
    wrong_source.source.sha256 = "0".repeat(64);
    reject_physical("unreviewed allocator source", wrong_source);
    let mut wrong_tls = physical.clone();
    wrong_tls.other_tls_getters[0].cell_offset += 8;
    reject_physical("different TLS cell", wrong_tls);
    let mut missing_getter = physical.clone();
    missing_getter.other_tls_getters.pop().unwrap();
    reject_physical("incomplete TLS getter census", missing_getter);
    let mut installation_writer = physical.clone();
    installation_writer.installation_writer_count = 1;
    reject_physical("an installation-depth writer", installation_writer);
    let mut writable_code = physical.clone();
    writable_code.mappings[0].flags |= program_header::PF_W;
    reject_physical("writable executable census mapping", writable_code);
    let mut wrong_allocation = physical.clone();
    wrong_allocation.allocation_sha256 = "0".repeat(64);
    reject_physical("different allocator body", wrong_allocation);

    // These deliberately foreign objects are refusal probes, not a definition
    // of a native receipt schema. No native record may enter the Cargo arm.
    let historical = serde_json::to_value(proof).unwrap();
    let mut replaced_build = historical.clone();
    replaced_build
        .as_object_mut()
        .unwrap()
        .remove("current_build");
    replaced_build["native_build"] = serde_json::json!({"status": 0});
    assert!(
        serde_json::from_value::<AllocatorInstallationEvidence>(replaced_build).is_err(),
        "native-shaped build evidence entered the historical Cargo arm"
    );
    let mut mixed = historical.clone();
    mixed["native_build"] = serde_json::json!({"status": 0});
    assert!(
        serde_json::from_value::<AllocatorInstallationEvidence>(mixed).is_err(),
        "the Cargo reader accepted a mixed native/Cargo envelope"
    );
    assert!(
        serde_json::from_value::<AllocatorInstallationEvidence>(serde_json::json!({
            "kind": "native_source",
            "physical": historical,
        }))
        .is_err(),
        "a native outer envelope entered the historical allocator arm"
    );
}

#[test]
fn native_authority_exact_cohort_controls() {
    fn select(roles: &BTreeMap<String, (Vec<String>, Value)>) -> Result<NoScopeCohort> {
        let borrowed = roles
            .iter()
            .map(|(name, (features, profile))| (name.as_str(), (features.as_slice(), profile)))
            .collect();
        select_no_scope_cohort(&borrowed)
    }
    let ordinary_profile = serde_json::json!({
        "opt_level": "3", "debuginfo": 0, "debug_assertions": true,
        "overflow_checks": true, "test": false,
    });
    let release_profile = serde_json::json!({
        "opt_level": "3", "debuginfo": 0, "debug_assertions": false,
        "overflow_checks": false, "test": false,
    });
    for (names, profile, expected) in [
        (
            vec!["detcore-liteinst", "reverie-liteinst", "reverie-inguest"],
            ordinary_profile,
            NoScopeCohort::DetcoreOrdinary101,
        ),
        (
            vec![
                "hermit-liteinst-runtime-artifact",
                "reverie-liteinst-preload",
                "reverie-liteinst",
                "reverie-inguest",
            ],
            release_profile,
            NoScopeCohort::StandaloneRelease62,
        ),
    ] {
        let roles: BTreeMap<String, (Vec<String>, Value)> = names
            .iter()
            .map(|name| {
                let features = if *name == "reverie-inguest" {
                    ["coordinator-rpc", "launcher", "rcb-clock"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect()
                } else {
                    Vec::new()
                };
                ((*name).to_owned(), (features, profile.clone()))
            })
            .collect();
        assert_eq!(select(&roles).unwrap(), expected);

        let mut missing_dependency = roles.clone();
        missing_dependency.remove("reverie-liteinst").unwrap();
        assert!(
            select(&missing_dependency).is_err(),
            "{expected:?}: missing role"
        );
        let mut extra_role = roles.clone();
        extra_role.insert(
            "unselected-dependency".into(),
            (Vec::new(), profile.clone()),
        );
        assert!(select(&extra_role).is_err(), "{expected:?}: extra role");
        let mut substituted_role = roles.clone();
        let dependency = substituted_role.remove("reverie-liteinst").unwrap();
        substituted_role.insert("lookalike-dependency".into(), dependency);
        assert!(
            select(&substituted_role).is_err(),
            "{expected:?}: substituted role"
        );

        let mut wrong_features = roles.clone();
        wrong_features.get_mut("reverie-inguest").unwrap().0.pop();
        assert!(
            select(&wrong_features).is_err(),
            "{expected:?}: dependency feature"
        );
        let mut repeated_feature = roles.clone();
        repeated_feature
            .get_mut("reverie-inguest")
            .unwrap()
            .0
            .push("rcb-clock".into());
        assert!(
            select(&repeated_feature).is_err(),
            "{expected:?}: duplicate feature"
        );
        let mut wrong_profile = roles.clone();
        wrong_profile.get_mut("reverie-liteinst").unwrap().1["test"] = serde_json::json!(true);
        assert!(
            select(&wrong_profile).is_err(),
            "{expected:?}: dependency test profile"
        );
    }
}
