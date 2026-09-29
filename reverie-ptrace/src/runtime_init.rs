/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Controller-bound images for explicit LiteInst runtime initialization.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Validated images and configuration for controller-owned LiteInst loading.
///
/// The controller must independently select the expected libc, interpreter and
/// libgcc_s bytes and public `dlopen` version. An observed guest pathname or
/// SONAME is insufficient to establish that trust. The runtime must export the ordinary host
/// initializer and the constructor-disabled host ABI marker, version one.
///
/// Construction validates the ELF images without executing their code. The
/// tracer compares the loader image with the stopped target and stages the
/// retained runtime bytes in a sealed target memfd at executable entry. It does
/// not load a mutable caller pathname or select a Tool from the environment.
/// Ptrace retains ownership of the sole Tool and GlobalTool.
pub struct LiteinstRuntimeInit {
    pub(crate) expected_loader: Vec<u8>,
    pub(crate) loader_version: String,
    pub(crate) expected_interpreter: Vec<u8>,
    pub(crate) expected_libgcc: Vec<u8>,
    pub(crate) runtime: Vec<u8>,
    pub(crate) mapping_path: PathBuf,
    pub(crate) config_words: [u64; 2],
}

impl LiteinstRuntimeInit {
    /// Binds and validates the loader provider, runtime, and explicit host config.
    ///
    /// `runtime` must be built with LiteInst's `preload-constructor` feature
    /// disabled. `straddler_staleness_ticks` is the calibrated WordPatch++ delay;
    /// zero disables concurrent cross-cache-line publication. A nonzero value
    /// does not authorize concurrent patch installation by the ptrace controller.
    ///
    /// Invalid or unsupported ELF images are rejected before a tracee is spawned.
    /// This operation requires Linux x86-64 and does not itself execute a guest.
    pub fn new(
        expected_loader: Vec<u8>,
        loader_version: String,
        expected_interpreter: Vec<u8>,
        expected_libgcc: Vec<u8>,
        runtime: Vec<u8>,
        straddler_staleness_ticks: u64,
    ) -> io::Result<Self> {
        #[cfg(target_arch = "x86_64")]
        let validation = crate::target_loader::validate_runtime_init_images(
            &expected_loader,
            &loader_version,
            &runtime,
        )
        .and_then(|()| {
            crate::target_loader::validate_runtime_init_dependencies(
                &expected_loader,
                &expected_interpreter,
                &expected_libgcc,
                &runtime,
            )
        });
        #[cfg(not(target_arch = "x86_64"))]
        let validation: io::Result<()> = Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "controller-owned LiteInst initialization requires x86-64",
        ));
        validation?;
        Ok(Self {
            expected_loader,
            loader_version,
            expected_interpreter,
            expected_libgcc,
            runtime,
            mapping_path: PathBuf::from("/memfd:reverie-liteinst-runtime (deleted)"),
            config_words: [1, straddler_staleness_ticks],
        })
    }
}

impl fmt::Debug for LiteinstRuntimeInit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiteinstRuntimeInit")
            .field("expected_loader_bytes", &self.expected_loader.len())
            .field("loader_version", &self.loader_version)
            .field(
                "expected_interpreter_bytes",
                &self.expected_interpreter.len(),
            )
            .field("expected_libgcc_bytes", &self.expected_libgcc.len())
            .field("runtime_bytes", &self.runtime.len())
            .field("mapping_path", &self.mapping_path)
            .field("config_words", &self.config_words)
            .finish()
    }
}
