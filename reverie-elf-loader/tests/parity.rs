/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! These are ordinary Linux tests; no Hermit or Reverie backend is involved.
//! Both starts require ADDR_NO_RANDOMIZE. At the ptrace exec stop we replace
//! the 16 AT_RANDOM bytes with the same witness bytes before any instruction
//! executes. Nothing else in the compared stack is normalized.

mod support;

use support::Fixture;
use support::Mutation;
use support::Naming;
use support::TestResult;

macro_rules! layout_test {
    ($name:ident, $fixture:ident, $naming:ident) => {
        #[test]
        fn $name() {
            support::test(stringify!($name), || {
                support::layout_case(stringify!($name), Fixture::$fixture, Naming::$naming, None)
            });
        }
    };
}

layout_test!(pie_absolute, Pie, Absolute);
layout_test!(nonpie_absolute, NonPie, Absolute);
layout_test!(pie_relative, Pie, Relative);
layout_test!(nonpie_relative, NonPie, Relative);
layout_test!(pie_directory_descriptor, Pie, DirectoryDescriptor);
layout_test!(nonpie_directory_descriptor, NonPie, DirectoryDescriptor);
layout_test!(pie_empty_path_descriptor, Pie, EmptyPath);
layout_test!(nonpie_empty_path_descriptor, NonPie, EmptyPath);
layout_test!(pie_deleted_file, Pie, Deleted);
layout_test!(nonpie_deleted_file, NonPie, Deleted);
layout_test!(pie_memfd, Pie, Memfd);
layout_test!(nonpie_memfd, NonPie, Memfd);

#[test]
fn bin_true() {
    support::test("bin_true", || {
        support::layout_case("bin_true", Fixture::True, Naming::Absolute, None)
    });
}

#[test]
fn complete_initial_register_state() {
    support::test("complete_initial_register_state", || {
        for fixture in [Fixture::EntryPie, Fixture::EntryNonPie] {
            support::observer_case("complete_initial_register_state", fixture, Mutation::None)?;
        }
        Ok(())
    });
}

#[test]
fn finite_data_limit() {
    support::test("finite_data_limit", || {
        for fixture in [Fixture::Pie, Fixture::NonPie] {
            // This reaches a real brk failure well before the 1024-page probe
            // cap. The comparator requires the failure request and result to
            // match, and observes VmData after every traced syscall.
            support::layout_case(
                "finite_data_limit",
                fixture,
                Naming::Absolute,
                Some(2 * 1024 * 1024),
            )?;
        }
        Ok(())
    });
}

#[test]
fn finite_data_startup_admission_boundary() {
    support::test("finite_data_startup_admission_boundary", || {
        support::finite_startup_boundary()
    });
}

#[test]
fn path_max_relative_and_directory_descriptor() {
    support::test("path_max_relative_and_directory_descriptor", || {
        support::path_boundary()
    });
}

#[test]
fn mutation_controls() {
    support::test("mutation_controls", || -> TestResult {
        for (mutation, assertion) in [
            (Mutation::OmitPlaceholder, "heap_metadata"),
            (Mutation::SkipVdsoRelocation, "mapping_parity"),
            (Mutation::DirtyRegisters, "entry_registers"),
        ] {
            support::mutation_case(mutation, assertion)?;
        }
        Ok(())
    });
}
