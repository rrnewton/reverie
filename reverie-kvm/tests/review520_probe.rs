// Untracked reviewer probe for the independent review of reverie PR 520.
// Not part of the PR; used only to measure guest-visible CPUID behaviour.

use std::path::PathBuf;

use reverie_kvm::KvmBackend;

const MEMORY: usize = 256 * 1024 * 1024;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let path =
            std::path::Path::new("/tmp").join(format!("review520-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

fn compile(dir: &std::path::Path, name: &str, source: &str, extra: &[&str]) -> PathBuf {
    let src = dir.join(format!("{name}.c"));
    let exe = dir.join(name);
    std::fs::write(&src, source).unwrap();
    let out = std::process::Command::new("/usr/bin/gcc")
        .args(["-O2", "-pthread"])
        .args(extra)
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gcc failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    exe
}

fn run_in_vm(exe: &std::path::Path, cwd: &std::path::Path) -> (i32, Vec<u8>, Vec<u8>) {
    let image = std::fs::read(exe).unwrap();
    let mut backend = KvmBackend::new(MEMORY).unwrap();
    let s = exe.to_str().unwrap();
    backend
        .install_static_elf_with_context(&image, &[s], &["PATH=/usr/bin:/bin"], cwd)
        .unwrap();
    backend.run_static_elf_captured().unwrap()
}

/// Dump every leaf-0xd subleaf the guest can ask for, plus a few control leaves.
#[test]
fn probe_dump_all_xstate_subleaves() {
    let dir = Dir::new("dump");
    let exe = compile(
        &dir.0,
        "dump-xstate",
        r#"
#include <cpuid.h>
#include <stdint.h>
#include <stdio.h>

int main(void) {
  uint32_t a, b, c, d;
  for (uint32_t i = 0; i <= 20; ++i) {
    __cpuid_count(0xd, i, a, b, c, d);
    printf("0xd.%u %08x %08x %08x %08x\n", i, a, b, c, d);
  }
  __cpuid_count(0xd, 63, a, b, c, d);
  printf("0xd.63 %08x %08x %08x %08x\n", a, b, c, d);
  __cpuid_count(1, 0, a, b, c, d);
  printf("0x1.0 %08x %08x %08x %08x\n", a, b, c, d);
  __cpuid_count(0x15, 0, a, b, c, d);
  printf("0x15.0 %08x %08x %08x %08x\n", a, b, c, d);
  __cpuid_count(0x15, 5, a, b, c, d);
  printf("0x15.5 %08x %08x %08x %08x\n", a, b, c, d);
  return 0;
}
"#,
        &["-fno-builtin", "-Wl,-z,lazy"],
    );
    let (code, stdout, stderr) = run_in_vm(&exe, &dir.0);
    println!("--- GUEST DUMP exit={code} ---");
    print!("{}", String::from_utf8_lossy(&stdout));
    eprint!("{}", String::from_utf8_lossy(&stderr));
    assert_eq!(code, 0);
}

/// A lazily bound `puts` with NO cpuid assertions: does the guest survive?
#[test]
fn probe_lazy_puts_only() {
    let dir = Dir::new("lazy");
    let exe = compile(
        &dir.0,
        "lazy-puts",
        r#"
#include <stdio.h>
int main(void) {
  if (puts("lazy puts ok") < 0) {
    return 20;
  }
  return 0;
}
"#,
        &["-fno-builtin", "-Wl,-z,lazy"],
    );
    let (code, stdout, stderr) = run_in_vm(&exe, &dir.0);
    println!(
        "--- LAZY PUTS exit={code} stdout={:?} stderr={:?} ---",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(code, 0);
}

/// What does the HOST report for leaf 0xd? Establishes whether the guest's
/// subleaf-0 EBX could have been host-derived on this box.
#[test]
fn probe_host_xstate_leaf() {
    let dir = Dir::new("host");
    let exe = compile(
        &dir.0,
        "host-xstate",
        r#"
#include <cpuid.h>
#include <stdint.h>
#include <stdio.h>
int main(void) {
  uint32_t a, b, c, d;
  for (uint32_t i = 0; i <= 3; ++i) {
    __cpuid_count(0xd, i, a, b, c, d);
    printf("host 0xd.%u %08x %08x %08x %08x\n", i, a, b, c, d);
  }
  return 0;
}
"#,
        &[],
    );
    let out = std::process::Command::new(&exe).output().unwrap();
    println!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(out.status.success());
}

/// Same program, but eagerly bound (`-z now`). Distinguishes a fault inside the
/// lazy PLT-resolution trampoline from a fault earlier in dynamic-loader setup.
#[test]
fn probe_eager_puts_only() {
    let dir = Dir::new("eager");
    let exe = compile(
        &dir.0,
        "eager-puts",
        r#"
#include <stdio.h>
int main(void) {
  if (puts("eager puts ok") < 0) {
    return 20;
  }
  return 0;
}
"#,
        &["-fno-builtin", "-Wl,-z,now"],
    );
    let (code, stdout, stderr) = run_in_vm(&exe, &dir.0);
    println!(
        "--- EAGER PUTS exit={code} stdout={:?} stderr={:?} ---",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(code, 0);
}

/// Which of the three new assertions actually has power on this toolchain?
/// Parses an eagerly bound binary and reports all three fields.
#[test]
fn probe_bind_now_markers_on_an_eager_binary() {
    let dir = Dir::new("bindnow");
    for (name, flag) in [("lazy", "-Wl,-z,lazy"), ("now", "-Wl,-z,now")] {
        let exe = compile(
            &dir.0,
            &format!("bn-{name}"),
            "#include <stdio.h>\nint main(void){ puts(\"x\"); return 0; }\n",
            &["-fno-builtin", flag],
        );
        let image = std::fs::read(&exe).unwrap();
        let elf = goblin::elf::Elf::parse(&image).unwrap();
        let dynamic = elf.dynamic.as_ref().unwrap();
        let has_dt = dynamic
            .dyns
            .iter()
            .any(|e| e.d_tag == goblin::elf::dynamic::DT_BIND_NOW);
        println!(
            "{name:>5}: DT_BIND_NOW={has_dt}  flags={:#x} (DF_BIND_NOW set: {})  flags_1={:#x} (DF_1_NOW set: {})",
            dynamic.info.flags,
            dynamic.info.flags & goblin::elf::dynamic::DF_BIND_NOW != 0,
            dynamic.info.flags_1,
            dynamic.info.flags_1 & goblin::elf::dynamic::DF_1_NOW != 0,
        );
    }
}
