/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::fs::File;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use reverie_elf_loader::AUXV_PATCH_TYPES;
use reverie_elf_loader::AuxvValue;
use reverie_elf_loader::Error;
use reverie_elf_loader::Invocation;
use reverie_elf_loader::Limits;
use reverie_elf_loader::MIN_PROGRAM_ADDRESS;
use reverie_elf_loader::PATH_MAX;
use reverie_elf_loader::pad_launcher_path;
use reverie_elf_loader::prepare_start;
use reverie_elf_loader::prepare_start_with_limits;
use sha2::Digest;
use sha2::Sha256;

fn timed(name: &str, work: impl FnOnce()) {
    let start = Instant::now();
    work();
    let path = Path::new(env!("ELF_LOADER_ARTIFACT_DIR")).join(format!("{name}.result"));
    fs::write(
        path,
        format!("PASS {name} {:.6}s\n", start.elapsed().as_secs_f64()),
    )
    .unwrap();
}

fn fixture() -> File {
    File::open(env!("ELF_LOADER_LAYOUT_NONPIE")).unwrap()
}

#[test]
fn padded_path_path_max_boundary() {
    timed("padded_path_path_max_boundary", || {
        let path = pad_launcher_path(Path::new("./h"), PATH_MAX - 1).unwrap();
        assert_eq!(path.as_bytes_with_nul().len(), PATH_MAX);
        assert!(matches!(
            pad_launcher_path(Path::new("./h"), PATH_MAX),
            Err(Error::PaddedPathTooLong {
                length_with_nul: 4097,
                maximum: 4096
            })
        ));
        // A legal 4095-byte relative path produces a longer kernel execfn when
        // execveat prefixes it with its directory descriptor.
        let relative = format!(".{}fixture", "/".repeat(PATH_MAX - 1 - 1 - 7));
        assert_eq!(relative.len(), PATH_MAX - 1);
        let invocation = Invocation::execveat(12, relative, 0).unwrap();
        let filename_length = invocation.native_execfn().as_bytes().len();
        assert_eq!(filename_length, PATH_MAX - 1 + "/dev/fd/12/".len());
        assert!(matches!(
            prepare_start(&fixture(), &invocation, Path::new("./h")),
            Err(Error::PaddedPathTooLong {
                length_with_nul: 4107,
                maximum: 4096
            })
        ));
        let invocation =
            Invocation::execve(format!(".{}fixture", "/".repeat(PATH_MAX - 9))).unwrap();
        assert_eq!(
            prepare_start(&fixture(), &invocation, Path::new("./h"))
                .unwrap()
                .padded_path
                .as_bytes_with_nul()
                .len(),
            PATH_MAX
        );
    });
}

#[test]
fn program_load_address_boundary() {
    timed("program_load_address_boundary", || {
        let invocation = Invocation::execve("./fixture").unwrap();
        let accepted = prepare_start(&fixture(), &invocation, Path::new("./h")).unwrap();
        assert_eq!(accepted.layout.program_base, MIN_PROGRAM_ADDRESS);
        let rejected = File::open(env!("ELF_LOADER_LOW_TARGET")).unwrap();
        assert!(matches!(
            prepare_start(&rejected, &invocation, Path::new("./h")),
            Err(Error::LowLoadSegment {
                address: 0x3ff000,
                interpreter: false
            })
        ));
        let pie = File::open(env!("ELF_LOADER_LAYOUT_PIE")).unwrap();
        let accepted = prepare_start(&pie, &invocation, Path::new("./h")).unwrap();
        assert_eq!(accepted.layout.program_base, 0x555555554000);
    });
}

#[test]
fn invocation_execfn_and_auxv_patch_contract() {
    timed("invocation_execfn_and_auxv_patch_contract", || {
        assert_eq!(
            Invocation::execve("./fixture")
                .unwrap()
                .native_execfn()
                .as_bytes(),
            b"./fixture"
        );
        assert_eq!(
            Invocation::execveat(12, "fixture", 0)
                .unwrap()
                .native_execfn()
                .as_bytes(),
            b"/dev/fd/12/fixture"
        );
        assert_eq!(
            Invocation::execveat(12, "", libc::AT_EMPTY_PATH)
                .unwrap()
                .native_execfn()
                .as_bytes(),
            b"/dev/fd/12"
        );
        assert_eq!(
            Invocation::execveat(12, "/bin/true", 0)
                .unwrap()
                .native_execfn()
                .as_bytes(),
            b"/bin/true"
        );
        let invocation = Invocation::execve("./custom-name").unwrap();
        let prepared = prepare_start(&fixture(), &invocation, Path::new("./h")).unwrap();
        assert_eq!(prepared.comm.as_bytes(), b"custom-name");
        let scalar_types: Vec<_> = prepared
            .auxv_patches
            .iter()
            .filter(|p| p.value != AuxvValue::ExecFnString)
            .map(|p| p.kind)
            .collect();
        assert_eq!(scalar_types, AUXV_PATCH_TYPES);
        assert_eq!(prepared.metadata(), b"./custom-name\0custom-name\0\0");
        assert!(matches!(
            Invocation::execveat(12, "", 0),
            Err(Error::InvalidInvocation(_))
        ));
        assert!(matches!(
            Invocation::execve("a\0b"),
            Err(Error::InvalidInvocation(_))
        ));
    });
}

#[test]
fn finite_address_space_refusal_boundary() {
    timed("finite_address_space_refusal_boundary", || {
        let invocation = Invocation::execve("./fixture").unwrap();
        let limits = Limits {
            data: libc::RLIM_INFINITY,
            address_space: libc::RLIM_INFINITY,
        };
        prepare_start_with_limits(&fixture(), &invocation, Path::new("./h"), limits).unwrap();
        for limit in [0, 4 << 20, libc::RLIM_INFINITY - 1] {
            let limits = Limits {
                address_space: limit,
                ..limits
            };
            assert!(
                matches!(prepare_start_with_limits(&fixture(), &invocation, Path::new("./h"), limits),
                Err(Error::FiniteAddressSpaceLimit { limit: observed }) if observed == limit)
            );
        }
        // DATA is admitted even when finite; execution/accounting parity is
        // exercised by the syscall-traced finite-limit integration test.
        prepare_start_with_limits(
            &fixture(),
            &invocation,
            Path::new("./h"),
            Limits {
                data: 4 << 20,
                ..limits
            },
        )
        .unwrap();
    });
}

#[test]
fn shadow_image_is_sparse_and_carries_native_data_fields() {
    timed(
        "shadow_image_is_sparse_and_carries_native_data_fields",
        || {
            let invocation = Invocation::execve("./fixture").unwrap();
            let prepared = prepare_start(&fixture(), &invocation, Path::new("./h")).unwrap();
            let path = scratch("shadow-image.elf");
            let output = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            prepared.write_image(&output).unwrap();
            record_binary(
                &path,
                "PreparedStart::write_image; source=layout-nonpie; invocation=./fixture",
            );
            let bytes = fs::read(&path).unwrap();
            let phoff = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
            let phnum = u16::from_le_bytes(bytes[56..58].try_into().unwrap()) as usize;
            let headers: Vec<_> = (0..phnum)
                .map(|index| &bytes[phoff + index * 56..phoff + (index + 1) * 56])
                .collect();
            let shadow = headers
                .iter()
                .find(|header| {
                    u32::from_le_bytes(header[..4].try_into().unwrap()) == 1
                        && u64::from_le_bytes(header[16..24].try_into().unwrap())
                            == prepared.layout.start_data
                })
                .unwrap();
            assert_eq!(u32::from_le_bytes(shadow[4..8].try_into().unwrap()), 4);
            assert_eq!(
                u64::from_le_bytes(shadow[32..40].try_into().unwrap()),
                prepared.layout.end_data - prepared.layout.start_data
            );
            assert_eq!(
                u64::from_le_bytes(shadow[40..48].try_into().unwrap()),
                prepared.layout.start_brk - prepared.layout.start_data
            );
            let offset = u64::from_le_bytes(shadow[8..16].try_into().unwrap()) as usize;
            assert!(bytes[offset..].iter().all(|byte| *byte == 0));
            assert_eq!(offset % 4096, prepared.layout.start_data as usize % 4096);
            use std::os::unix::fs::MetadataExt;
            assert!(output.metadata().unwrap().blocks() * 512 < output.metadata().unwrap().len());
            assert!(matches!(
                prepared.write_image_from_template(&output, b"not an ELF"),
                Err(Error::InvalidLoaderTemplate(_))
            ));
        },
    );
}

#[test]
fn malformed_and_static_elf_refusals() {
    timed("malformed_and_static_elf_refusals", || {
        let invocation = Invocation::execve("./fixture").unwrap();
        let source = fs::read(env!("ELF_LOADER_LAYOUT_NONPIE")).unwrap();
        let path = scratch("malformed.elf");
        for (offset, value, expected) in [(4, 1, "class"), (54, 55, "phsize")] {
            let mut image = source.clone();
            image[offset] = value;
            fs::write(&path, image).unwrap();
            record_binary(
                &path,
                &format!("layout-nonpie; ELF header byte[{offset}]={value}"),
            );
            assert!(
                matches!(
                    prepare_start(&File::open(&path).unwrap(), &invocation, Path::new("./h")),
                    Err(Error::UnsupportedElf(_))
                ),
                "{expected}"
            );
        }
        let mut image = source;
        let phoff = u64::from_le_bytes(image[32..40].try_into().unwrap()) as usize;
        let phnum = u16::from_le_bytes(image[56..58].try_into().unwrap()) as usize;
        for index in 0..phnum {
            let at = phoff + index * 56;
            if u32::from_le_bytes(image[at..at + 4].try_into().unwrap()) == 3 {
                image[at..at + 4].copy_from_slice(&0_u32.to_le_bytes());
            }
        }
        let mut output = File::create(&path).unwrap();
        output.write_all(&image).unwrap();
        record_binary(
            &path,
            "layout-nonpie; change every PT_INTERP p_type to PT_NULL",
        );
        assert!(matches!(
            prepare_start(&File::open(&path).unwrap(), &invocation, Path::new("./h")),
            Err(Error::MissingInterpreter)
        ));
    });
}

#[test]
fn descriptor_comm_ambiguity_is_named() {
    timed("descriptor_comm_ambiguity_is_named", || {
        let invocation = Invocation::execveat(12, "", libc::AT_EMPTY_PATH).unwrap();
        let path = scratch("x (deleted)");
        fs::copy(env!("ELF_LOADER_LAYOUT_NONPIE"), &path).unwrap();
        record_binary(&path, "copy layout-nonpie as x (deleted)");
        let pinned = File::open(&path).unwrap();
        assert!(matches!(
            prepare_start(&pinned, &invocation, Path::new("./h")),
            Err(Error::AmbiguousDescriptorComm)
        ));
        fs::remove_file(&path).unwrap();
        let accepted = prepare_start(&pinned, &invocation, Path::new("./h")).unwrap();
        assert_eq!(accepted.comm.as_bytes(), b"x (deleted)");

        let path = scratch("linked-live");
        let alias = scratch("linked-alias");
        if alias.exists() {
            fs::remove_file(&alias).unwrap();
        }
        fs::copy(env!("ELF_LOADER_LAYOUT_NONPIE"), &path).unwrap();
        fs::hard_link(&path, &alias).unwrap();
        record_binary(
            &path,
            "copy layout-nonpie as linked-live; hard_link linked-alias",
        );
        let pinned = File::open(&path).unwrap();
        let accepted = prepare_start(&pinned, &invocation, Path::new("./h")).unwrap();
        assert_eq!(accepted.comm.as_bytes(), b"linked-live");
        fs::remove_file(path).unwrap();
        assert!(matches!(
            prepare_start(&pinned, &invocation, Path::new("./h")),
            Err(Error::AmbiguousDescriptorComm)
        ));
        fs::remove_file(alias).unwrap();
        let accepted = prepare_start(&pinned, &invocation, Path::new("./h")).unwrap();
        assert_eq!(accepted.comm.as_bytes(), b"linked-live");
    });
}

#[test]
fn stack_header_refusals_match_loader_contract() {
    timed("stack_header_refusals_match_loader_contract", || {
        let invocation = Invocation::execve("./fixture").unwrap();
        let source = fs::read(env!("ELF_LOADER_LAYOUT_NONPIE")).unwrap();
        prepare_start(&fixture(), &invocation, Path::new("./h")).unwrap();
        let phoff = u64::from_le_bytes(source[32..40].try_into().unwrap()) as usize;
        let phnum = u16::from_le_bytes(source[56..58].try_into().unwrap()) as usize;
        let note_index = (0..phnum)
            .find(|index| {
                u32::from_le_bytes(
                    source[phoff + index * 56..phoff + index * 56 + 4]
                        .try_into()
                        .unwrap(),
                ) == 4
            })
            .unwrap();
        for flags in [6_u32, 7] {
            let mut image = source.clone();
            let at = phoff + note_index * 56;
            image[at..at + 4].copy_from_slice(&0x6474e551_u32.to_le_bytes());
            image[at + 4..at + 8].copy_from_slice(&flags.to_le_bytes());
            let path = scratch(&format!("stack-flags-{flags}.elf"));
            fs::write(&path, image).unwrap();
            record_binary(
                &path,
                &format!("layout-nonpie; first PT_NOTE becomes PT_GNU_STACK flags={flags}"),
            );
            assert!(matches!(
                prepare_start(&File::open(path).unwrap(), &invocation, Path::new("./h")),
                Err(Error::ExecutableStack)
            ));
        }
    });
}

#[test]
fn raw_and_biased_user_address_bounds() {
    timed("raw_and_biased_user_address_bounds", || {
        for (source, raw, reason) in [
            (
                env!("ELF_LOADER_LAYOUT_NONPIE"),
                true,
                "PT_LOAD exceeds x86-64 default user address range",
            ),
            (
                env!("ELF_LOADER_LAYOUT_PIE"),
                false,
                "biased PT_LOAD exceeds x86-64 default user address range",
            ),
        ] {
            let mut image = fs::read(source).unwrap();
            let phoff = u64::from_le_bytes(image[32..40].try_into().unwrap()) as usize;
            let phnum = u16::from_le_bytes(image[56..58].try_into().unwrap()) as usize;
            let last_load = (0..phnum)
                .rfind(|index| {
                    u32::from_le_bytes(
                        image[phoff + index * 56..phoff + index * 56 + 4]
                            .try_into()
                            .unwrap(),
                    ) == 1
                })
                .unwrap();
            let at = phoff + last_load * 56;
            let original = u64::from_le_bytes(image[at + 16..at + 24].try_into().unwrap());
            let address = if raw {
                0x800000000000_u64
            } else {
                0x7fffffffe000_u64
            } + original % 4096;
            image[at + 16..at + 24].copy_from_slice(&address.to_le_bytes());
            if !raw {
                let size = 8192 - original % 4096;
                image[at + 40..at + 48].copy_from_slice(&size.to_le_bytes());
            }
            let path = scratch(&format!("address-bound-{raw}.elf"));
            fs::write(&path, image).unwrap();
            record_binary(
                &path,
                &format!("source={source}; last PT_LOAD vaddr={address:#x}; raw={raw}"),
            );
            assert!(
                matches!(prepare_start(&File::open(path).unwrap(), &Invocation::execve("./fixture").unwrap(), Path::new("./h")),
                Err(Error::UnsupportedElf(observed)) if observed == reason)
            );
        }
    });
}

#[test]
fn nonregular_interpreter_is_refused_without_opening_it_for_read() {
    timed(
        "nonregular_interpreter_is_refused_without_opening_it_for_read",
        || {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let fifo = scratch("interpreter.fifo");
            if fifo.exists() {
                fs::remove_file(&fifo).unwrap();
            }
            let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
            // SAFETY: name is a terminated pathname owned by this test.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let mut image = fs::read(env!("ELF_LOADER_LAYOUT_NONPIE")).unwrap();
            let phoff = u64::from_le_bytes(image[32..40].try_into().unwrap()) as usize;
            let phnum = u16::from_le_bytes(image[56..58].try_into().unwrap()) as usize;
            let index = (0..phnum)
                .find(|index| {
                    u32::from_le_bytes(
                        image[phoff + index * 56..phoff + index * 56 + 4]
                            .try_into()
                            .unwrap(),
                    ) == 3
                })
                .unwrap();
            let at = phoff + index * 56;
            let offset = image.len() as u64;
            let size = name.as_bytes_with_nul().len() as u64;
            image[at + 8..at + 16].copy_from_slice(&offset.to_le_bytes());
            image[at + 32..at + 40].copy_from_slice(&size.to_le_bytes());
            image.extend_from_slice(name.as_bytes_with_nul());
            let path = scratch("fifo-interpreter-target.elf");
            fs::write(&path, image).unwrap();
            record_binary(
                &path,
                "layout-nonpie; append interpreter.fifo pathname and point PT_INTERP to it",
            );
            let target = File::open(path).unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = prepare_start(
                    &target,
                    &Invocation::execve("./fixture").unwrap(),
                    Path::new("./h"),
                );
                let _ = send.send(result);
            });
            let result = receive
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("prepare_start blocked on a FIFO interpreter");
            worker.join().unwrap();
            assert!(matches!(
                result,
                Err(Error::UnsupportedElf("interpreter is not a regular file"))
            ));
            fs::remove_file(fifo).unwrap();
        },
    );
}

fn scratch(name: &str) -> PathBuf {
    Path::new(env!("ELF_LOADER_ARTIFACT_DIR")).join(name)
}

fn record_binary(path: &Path, operation: &str) {
    let sha = Sha256::digest(fs::read(path).unwrap());
    let entry = format!(
        "generation: cargo --offline test -p reverie-elf-loader --test preparation; operation={operation}\nsha256: {sha:x}  {}\n",
        path.display()
    );
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(scratch("preparation-images.provenance"))
        .unwrap();
    output.write_all(entry.as_bytes()).unwrap();
}
