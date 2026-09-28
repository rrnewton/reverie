// SOURCE /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/parity-composition-v1/source/reverie-kvm/src/lib.rs lines 9..33; full file SHA256 f6004c9725e8085056b1ca99dc91fd7c2a5fcdd912f84d32a082a4ed3a3a6262
//! Minimal x86-64 KVM primitives for experimenting with a Reverie backend.
//!
//! The crate supports the original real-mode transport probes and a bare
//! long-mode process personality for fixed-address static ELF executables. The
//! latter implements only a bounded, single-process subset of Linux semantics;
//! see the crate README for its explicit limits.

#![cfg(target_arch = "x86_64")]

mod bootstrap;
mod clock;
mod cpuid;
mod cpuid_instruction;
mod elf;
mod error;
mod executor;
mod failure;
mod fdinfo;
mod memory;
mod proc_mounts;
mod runtime;
mod signal;
mod stats;
mod syscall;
mod timestamp;

