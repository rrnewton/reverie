# Current source locations

These are author source pointers, not independent findings or runtime results. Every source file is retained under this packet’s source directory; the complete changed-file predecessor is under evidence/before.

- CPL3 admission before any user entry: `source/reverie-kvm/src/runtime.rs:2933`.
- timestamp callback and supervised outcomes: `source/reverie-kvm/src/runtime.rs:3156`.
- returning/nonreturning injection admission: `source/reverie-kvm/src/runtime.rs:455`.
- signal/mask pre-effect guard: `source/reverie-kvm/src/runtime.rs:580`.
- actual saved exception frame: `source/reverie-kvm/src/vm.rs:2615`.
- subscribed fault recognition: `source/reverie-kvm/src/vm.rs:2643`.
- register retirement and single-step: `source/reverie-kvm/src/vm.rs:2713`.
- tool-less Host worker loop: `source/reverie-kvm/src/vm.rs:3199`.
- public direct tool-less loop: `source/reverie-kvm/src/vm.rs:3560`.
- actual clock interval ends at exit: `source/reverie-kvm/src/clock.rs:112`.
- instruction length and prefixes: `source/reverie-kvm/src/timestamp.rs:28`.
- executable-page access: `source/reverie-kvm/src/timestamp.rs:81`.
- new actual page-boundary control: `source/reverie-kvm/src/vm.rs:6382`.
- all historical and new static controls: `source/reverie-kvm/tests/static_elf.rs:13928`.

The public `run_with_tool` definition is in `source/reverie-kvm/src/runtime.rs`; it is unchanged. The syscall and real-mode installation paths and new-vCPU constructors are in `source/reverie-kvm/src/vm.rs`; their original consumer selection remains in force. The full unchanged Hermit callback is retained as `evidence/hermit-detcore-lib.rs:1349`; this is source context, not a candidate Hermit binary binding.
