use super::*;
use crate::owned_step::native::Refusal as StepRefusal;
use crate::vdso::Active;
use crate::vdso::Mapping;

fn descriptor() -> Vec<u8> {
    std::fs::read(
        std::env::var_os("LITEINST_RNG_DESCRIPTOR_ELF").expect("explicit descriptor fixture"),
    )
    .expect("read retained descriptor fixture")
}

pub(crate) fn active(base: u64) -> Active {
    active_for_owner(base, 17)
}

pub(crate) fn active_for_owner(base: u64, owner: i64) -> Active {
    let bytes = descriptor();
    let maps = [
        (base - 0x6000..base - 0x2000, "[vvar]", libc::PROT_READ),
        (base - 0x2000..base, "[vvar_vclock]", libc::PROT_READ),
        (
            base..base + 8192,
            "[vdso]",
            libc::PROT_READ | libc::PROT_EXEC,
        ),
    ]
    .into_iter()
    .map(|(range, name, protection)| Mapping {
        range,
        protection,
        offset: 0,
        device: (0, 0),
        inode: 0,
        name: name.into(),
    })
    .collect();
    Active {
        owner,
        kernel_mappings: maps,
        range: base..base + 8192,
        vvar: vec![base - 0x6000..base - 0x2000, base - 0x2000..base],
        exports: crate::vdso::elf::exports(&bytes, base..base + 8192).unwrap(),
        writable: Vec::new(),
        changes: Vec::new(),
        _image: bytes,
    }
}

fn context(pc: u64) -> HookContext {
    let mut context: HookContext = unsafe { core::mem::zeroed() };
    context.instruction_pointer = pc;
    context.rflags = 0x202;
    context.rax = 0xfedcba9876543210;
    context.rcx = 0x1122334455667788;
    context.r14 = 0x8877665544332211;
    context
}

fn identity() -> ReadIdentity {
    ReadIdentity {
        owner: 17,
        frame_generation: 9,
        cpu_sequence: 31,
    }
}

fn cpu(active: &Active, context: &HookContext) -> CpuStepRequest {
    let offset = (context.instruction_pointer - active.range.start) as usize;
    CpuStepRequest::decode_recorded(
        context.instruction_pointer,
        &active._image[offset..],
        context,
        false,
        &mut StepRefusal::new("test"),
    )
    .unwrap()
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn digest_standard_vectors_and_retained_descriptor() {
    for (input, expected) in [
        (
            "",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            "abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        ),
    ] {
        assert_eq!(hex(digest::sha256(input.as_bytes())), expected);
    }
    assert_eq!(digest::sha256(&descriptor()), IMAGE_SHA256);
}

#[test]
fn all_retained_rng_forms_relocate_and_preserve_input_metadata() {
    for base in [0x100000, 0x7ffff7ffd000] {
        let owner = active(base);
        let binding = owner.rng_binding().unwrap();
        for (offset, field, code, register, immediate) in [
            (0x10be, RngField::Ready, Code::Cmp_rm8_imm8, None, Some(0)),
            (
                0x110b,
                RngField::Generation,
                Code::Mov_r64_rm64,
                Some(Register::RCX),
                None,
            ),
            (
                0x13b3,
                RngField::Generation,
                Code::Cmp_r64_rm64,
                Some(Register::RAX),
                None,
            ),
        ] {
            let context = context(base + offset);
            let planned = cpu(&owner, &context);
            let before = planned.clone();
            let request = binding.decode(identity(), planned, &context).unwrap();
            assert_eq!(request.identity(), identity());
            assert_eq!(request.image_sha256(), IMAGE_SHA256);
            assert_eq!(request.field(), field);
            assert_eq!(
                request.address(),
                base - 0x4000 + if field == RngField::Ready { 8 } else { 0 }
            );
            assert_eq!(request.code(), code);
            assert_eq!(request.register(), register);
            assert_eq!(request.immediate(), immediate);
            assert_eq!(request.next_pc(), base + offset + 7);
            assert_eq!(request.cpu_request(), &before);
            assert_eq!(request.cpu_request().input_flags(), 0x202);
            assert_eq!(
                request.cpu_request().input_registers(),
                before.input_registers()
            );
            assert_eq!(request.validate(&binding, identity(), &context), Ok(()));
        }
    }
}

#[test]
fn image_hash_covers_headers_code_and_noninstruction_bytes() {
    for offset in [0, 0x10be, 8191] {
        let mut owner = active(0x100000);
        owner._image[offset] ^= 1;
        assert_eq!(owner.rng_binding().unwrap_err(), Refusal::Image);
    }
    let mut owner = active(0x100000);
    owner._image.pop();
    assert_eq!(owner.rng_binding().unwrap_err(), Refusal::Image);
}

#[test]
fn named_mapping_geometry_permissions_and_identity_are_required() {
    for mutation in 0..9 {
        let mut owner = active(0x100000);
        match mutation {
            0 => owner.kernel_mappings[0].name = "[ordinary]".into(),
            1 => owner.kernel_mappings[0].range.start += 4096,
            2 => owner.kernel_mappings[1].range.end -= 4096,
            3 => owner.kernel_mappings[0].protection |= libc::PROT_WRITE,
            4 => owner.kernel_mappings[0].device = (1, 0),
            5 => owner.kernel_mappings[0].inode = 1,
            6 => owner.kernel_mappings[0].offset = 4096,
            7 => owner.vvar[0].start += 4096,
            8 => owner.kernel_mappings.swap(0, 1),
            _ => unreachable!(),
        }
        assert_eq!(
            owner.rng_binding().unwrap_err(),
            Refusal::Layout,
            "mutation {mutation}"
        );
    }
}

#[test]
fn retained_exports_and_real_owner_are_required() {
    for mutation in 0..3 {
        let mut owner = active(0x100000);
        let export = owner
            .exports
            .iter_mut()
            .find(|export| export.name == "getrandom")
            .unwrap();
        match mutation {
            0 => export.version = "LINUX_2.7".into(),
            1 => export.address += 1,
            2 => export.name = "unqualified".into(),
            _ => unreachable!(),
        }
        assert_eq!(owner.rng_binding().unwrap_err(), Refusal::Export);
    }
    let mut owner = active(0x100000);
    owner.owner = 0;
    assert_eq!(owner.rng_binding().unwrap_err(), Refusal::Owner);
}

#[test]
fn decode_and_revalidation_reject_stale_owner_generation_sequence_and_state() {
    let owner = active(0x100000);
    let binding = owner.rng_binding().unwrap();
    let initial = context(0x10110b);
    let planned = cpu(&owner, &initial);
    let request = binding
        .decode(identity(), planned.clone(), &initial)
        .unwrap();
    for changed in [
        ReadIdentity {
            owner: 18,
            ..identity()
        },
        ReadIdentity {
            frame_generation: 0,
            ..identity()
        },
        ReadIdentity {
            cpu_sequence: 0,
            ..identity()
        },
    ] {
        assert_eq!(
            binding
                .decode(changed, planned.clone(), &initial)
                .unwrap_err(),
            Refusal::Owner
        );
    }
    for changed in [
        ReadIdentity {
            owner: 18,
            ..identity()
        },
        ReadIdentity {
            frame_generation: 10,
            ..identity()
        },
        ReadIdentity {
            cpu_sequence: 32,
            ..identity()
        },
    ] {
        assert_eq!(
            request.validate(&binding, changed, &initial),
            Err(Refusal::Owner)
        );
    }
    for mutation in 0..4 {
        let mut changed = context(initial.instruction_pointer);
        match mutation {
            0 => changed.rax ^= 1,
            1 => changed.r14 ^= 1,
            2 => changed.rflags ^= 0x400,
            3 => changed.instruction_pointer += 1,
            _ => unreachable!(),
        }
        assert_eq!(
            request.validate(&binding, identity(), &changed),
            Err(Refusal::State)
        );
        assert_eq!(
            binding
                .decode(identity(), planned.clone(), &changed)
                .unwrap_err(),
            Refusal::State
        );
    }
    let relocated = active(0x200000);
    assert_eq!(
        request.validate(&relocated.rng_binding().unwrap(), identity(), &initial),
        Err(Refusal::Owner)
    );
}

#[test]
fn different_recorded_instruction_and_unrelated_field_pc_refused() {
    let owner = active(0x100000);
    let binding = owner.rng_binding().unwrap();
    let initial = context(0x10110b);
    let altered = CpuStepRequest::decode_recorded(
        initial.instruction_pointer,
        &[0x48, 0x8b, 0x05, 0xee, 0xae, 0xff, 0xff],
        &initial,
        false,
        &mut StepRefusal::new("test"),
    )
    .unwrap();
    assert_eq!(
        binding.decode(identity(), altered, &initial).unwrap_err(),
        Refusal::Instruction
    );
    let initial = context(0x101050);
    assert_eq!(
        binding
            .decode(identity(), cpu(&owner, &initial), &initial)
            .unwrap_err(),
        Refusal::Field
    );
}

#[test]
fn operand_width_address_write_prefix_and_mixed_memory_refused() {
    let pc = 0x10110b;
    let valid = Decoder::with_ip(
        64,
        &[0x48, 0x8b, 0x0d, 0xee, 0xae, 0xff, 0xff],
        pc,
        DecoderOptions::NONE,
    )
    .decode();
    assert_eq!(
        validate_operand(
            &valid,
            RngField::Generation,
            0xfc000,
            Some(Register::RCX),
            None
        ),
        Ok(())
    );
    assert_eq!(
        validate_operand(&valid, RngField::Ready, 0xfc000, Some(Register::RCX), None),
        Err(Refusal::Operand)
    );
    assert_eq!(
        validate_operand(
            &valid,
            RngField::Generation,
            0xfc008,
            Some(Register::RCX),
            None
        ),
        Err(Refusal::Operand)
    );
    assert_eq!(
        validate_operand(
            &valid,
            RngField::Generation,
            0xfc000,
            Some(Register::RAX),
            None
        ),
        Err(Refusal::Operand)
    );
    for bytes in [
        &[0x48, 0x89, 0x0d, 0xee, 0xae, 0xff, 0xff][..],
        &[0x8b, 0x0d, 0xee, 0xae, 0xff, 0xff],
        &[0x64, 0x48, 0x8b, 0x0d, 0xee, 0xae, 0xff, 0xff],
        &[0xf3, 0x48, 0x8b, 0x0d, 0xee, 0xae, 0xff, 0xff],
        &[0xa6],
        &[0x48, 0x8b, 0x0f],
    ] {
        let instruction = Decoder::with_ip(64, bytes, pc, DecoderOptions::NONE).decode();
        assert_eq!(
            validate_operand(
                &instruction,
                RngField::Generation,
                instruction.ip_rel_memory_address(),
                Some(Register::RCX),
                None
            ),
            Err(Refusal::Operand)
        );
    }
}
