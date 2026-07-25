#!/usr/bin/env rust-script
/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Cross-backend syscall-counter validation.
//!
//! Runs the `reverie-sysctr-{ptrace,kvm,dbi}` binaries built by
//! `reverie-multibackend-tools` against controlled guest programs and checks
//! that the backends agree on syscall counts to the extent their architectures
//! allow. This is the executable form of task `impl-sysctr-cross-backend`.
//!
//! ## Why hand-assembled guests
//!
//! The KVM backend's tool runner loads a *raw-machine-code static ELF* at a
//! fixed address and starts executing at the entry point — it does not run a
//! libc program through an ELF interpreter. To compare the exact same guest
//! across ptrace and KVM, this harness emits raw-code static ELFs (identical
//! layout to `reverie-kvm/tests/static_elf.rs`) that both the Linux kernel
//! (ptrace path) and the KVM loader can execute. Their syscall counts are then
//! analytically known, so agreement is checkable against a closed form, not
//! just against each other.
//!
//! ## What is (and is not) asserted
//!
//! These are the *bare* Reverie backends plus a counting tool, NOT
//! `hermit --strict` / Detcore. Bitwise-identical counts across runs is a
//! Detcore determinism property (L2); at this layer we assert:
//!
//! * **Single-process, hard gate:** `kvm == guest-issued syscalls` exactly and
//!   `ptrace == kvm + 1`. The `+1` is the launching `execve` that ptrace
//!   observes and KVM has no equivalent for (it boots at the entry point).
//!   Both must be stable across every repeat.
//! * **Fork-tree, hard gate (ptrace):** the aggregated `process_count` equals
//!   `children + 1` on every repeat — proof the shared global state aggregates
//!   across the whole tree rather than fragmenting per-process.
//! * **Fork-tree, soft check (ptrace):** the aggregated syscall *total* is at
//!   least the analytic count and within a small tolerance above it. A modest
//!   positive wobble is expected from `wait4`/SIGCHLD restart races at the
//!   bare-ptrace layer (no Detcore), so an exact total is not a hard gate here.
//! * **KVM fork:** reported informationally — the `run_static_elf_with_tool`
//!   adapter runs a single process and does not surface a fork tree to the
//!   tool, so KVM cannot be a fork-tree aggregation witness today.
//! * **DBI:** skipped with an explicit reason unless a per-tool native client
//!   and `DYNAMORIO_HOME`/`REVERIE_DBI_CLIENT` are provided; even then DBI
//!   global state fragments across fork until the coordinator-RPC fix lands
//!   (task `impl-dbi-global-state-fix`).
//!
//! ## Usage
//!
//! ```text
//! validate-cross-backend.rs [--bin-dir DIR] [--repeats N] [--keep]
//! ```
//!
//! `--bin-dir` (or env `REVERIE_SYSCTR_BINDIR`) is where the
//! `reverie-sysctr-*` binaries live; it defaults to a short search of the
//! usual cargo target dirs. Exit status is non-zero if any hard gate fails.

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

// ------------------------------------------------------------------ guest gen

const LOAD_ADDRESS: u64 = 0x20_0000;
const CODE_OFFSET: usize = 0x1000;
/// getpid calls each child issues in the fork-tree guest.
const CHILD_GETPIDS: i32 = 4;

fn put_u16(b: &mut [u8], o: usize, v: u16) { b[o..o + 2].copy_from_slice(&v.to_le_bytes()); }
fn put_u32(b: &mut [u8], o: usize, v: u32) { b[o..o + 4].copy_from_slice(&v.to_le_bytes()); }
fn put_u64(b: &mut [u8], o: usize, v: u64) { b[o..o + 8].copy_from_slice(&v.to_le_bytes()); }

/// Wrap raw x86-64 code into a minimal `ET_EXEC` static ELF, byte-for-byte the
/// same layout `reverie-kvm/tests/static_elf.rs` uses. `p_vaddr - p_offset` is
/// page-aligned, so the Linux kernel can `execve` it directly too.
fn static_elf(code: &[u8]) -> Vec<u8> {
    let mut img = vec![0u8; CODE_OFFSET + code.len()];
    img[..4].copy_from_slice(b"\x7fELF");
    img[4] = 2; img[5] = 1; img[6] = 1;
    put_u16(&mut img, 16, 2);   // e_type = ET_EXEC
    put_u16(&mut img, 18, 62);  // e_machine = x86-64
    put_u32(&mut img, 20, 1);   // e_version
    put_u64(&mut img, 24, LOAD_ADDRESS); // e_entry
    put_u64(&mut img, 32, 64);  // e_phoff
    put_u16(&mut img, 52, 64);  // e_ehsize
    put_u16(&mut img, 54, 56);  // e_phentsize
    put_u16(&mut img, 56, 1);   // e_phnum
    put_u32(&mut img, 64, 1);   // p_type = PT_LOAD
    put_u32(&mut img, 68, 5);   // p_flags = R+X
    put_u64(&mut img, 72, CODE_OFFSET as u64); // p_offset
    put_u64(&mut img, 80, LOAD_ADDRESS);       // p_vaddr
    put_u64(&mut img, 88, LOAD_ADDRESS);       // p_paddr
    put_u64(&mut img, 96, code.len() as u64);  // p_filesz
    put_u64(&mut img, 104, 0x2000);            // p_memsz
    put_u64(&mut img, 112, 0x1000);            // p_align
    img[CODE_OFFSET..].copy_from_slice(code);
    img
}

/// `n` × getpid, then exit_group(0). Guest-issued syscalls = `n + 1`.
fn guest_single(n: u32) -> Vec<u8> {
    let mut c = Vec::new();
    for _ in 0..n {
        c.extend_from_slice(&[0xb8, 39, 0, 0, 0, 0x0f, 0x05]); // mov eax,39(getpid); syscall
    }
    c.extend_from_slice(&[0xb8, 231, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05, 0x0f, 0x0b]); // exit_group(0); ud2
    static_elf(&c)
}

/// Parent forks `n` children; each child does `m` getpid then exit_group; parent
/// wait4()s each then exit_group. r12/r13/r14 are callee-saved (untouched by the
/// `syscall` instruction) and used as counters.
///
/// Guest-issued syscalls = `n*m + 3n + 1`; distinct pids = `n + 1`.
fn guest_fork(n: i32, m: i32) -> Vec<u8> {
    let mut c: Vec<u8> = Vec::new();
    c.extend_from_slice(&[0x49, 0xC7, 0xC4]); c.extend_from_slice(&n.to_le_bytes()); // mov r12, n
    c.extend_from_slice(&[0x49, 0xC7, 0xC5]); c.extend_from_slice(&n.to_le_bytes()); // mov r13, n
    c.extend_from_slice(&[0x49, 0xC7, 0xC6]); c.extend_from_slice(&m.to_le_bytes()); // mov r14, m

    let spawn = c.len();
    c.extend_from_slice(&[0x4D, 0x85, 0xE4]);                     // test r12,r12
    c.extend_from_slice(&[0x74, 0x00]); let j_reap = c.len() - 1; // jz reap
    c.extend_from_slice(&[0xB8, 0x39, 0, 0, 0, 0x0F, 0x05]);      // fork; syscall
    c.extend_from_slice(&[0x48, 0x85, 0xC0]);                     // test rax,rax
    c.extend_from_slice(&[0x74, 0x00]); let j_child = c.len() - 1;// jz child_body
    c.extend_from_slice(&[0x49, 0xFF, 0xCC]);                     // dec r12
    c.extend_from_slice(&[0xEB, 0x00]); let jb = c.len() - 1;     // jmp spawn
    c[jb] = ((spawn as isize) - (jb as isize + 1)) as i8 as u8;

    let reap = c.len();
    c[j_reap] = ((reap as isize) - (j_reap as isize + 1)) as i8 as u8;
    let reap_loop = c.len();
    c.extend_from_slice(&[0x4D, 0x85, 0xED]);                     // test r13,r13
    c.extend_from_slice(&[0x74, 0x00]); let j_done = c.len() - 1; // jz done
    c.extend_from_slice(&[0xBF, 0xFF, 0xFF, 0xFF, 0xFF]);         // mov edi,-1
    c.extend_from_slice(&[0x31, 0xF6, 0x31, 0xD2, 0x45, 0x31, 0xD2]); // xor esi/edx/r10d
    c.extend_from_slice(&[0xB8, 0x3D, 0, 0, 0, 0x0F, 0x05]);      // wait4; syscall
    c.extend_from_slice(&[0x49, 0xFF, 0xCD]);                     // dec r13
    c.extend_from_slice(&[0xEB, 0x00]); let jbr = c.len() - 1;    // jmp reap_loop
    c[jbr] = ((reap_loop as isize) - (jbr as isize + 1)) as i8 as u8;

    let done = c.len();
    c[j_done] = ((done as isize) - (j_done as isize + 1)) as i8 as u8;
    c.extend_from_slice(&[0xB8, 0xE7, 0, 0, 0, 0x31, 0xFF, 0x0F, 0x05, 0x0F, 0x0B]); // exit_group(0);ud2

    let child = c.len();
    c[j_child] = ((child as isize) - (j_child as isize + 1)) as i8 as u8;
    let child_loop = c.len();
    c.extend_from_slice(&[0x4D, 0x85, 0xF6]);                     // test r14,r14
    c.extend_from_slice(&[0x74, 0x00]); let j_cx = c.len() - 1;   // jz child_exit
    c.extend_from_slice(&[0xB8, 0x27, 0, 0, 0, 0x0F, 0x05]);      // getpid; syscall
    c.extend_from_slice(&[0x49, 0xFF, 0xCE]);                     // dec r14
    c.extend_from_slice(&[0xEB, 0x00]); let jbc = c.len() - 1;    // jmp child_loop
    c[jbc] = ((child_loop as isize) - (jbc as isize + 1)) as i8 as u8;

    let cx = c.len();
    c[j_cx] = ((cx as isize) - (j_cx as isize + 1)) as i8 as u8;
    c.extend_from_slice(&[0xB8, 0xE7, 0, 0, 0, 0x31, 0xFF, 0x0F, 0x05, 0x0F, 0x0B]); // exit_group(0);ud2

    static_elf(&c)
}

fn write_guest(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(bytes).unwrap();
    drop(f);
    let mut p = std::fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&path, p).unwrap();
    path
}

// ----------------------------------------------------------------- run + parse

/// One observation from a sysctr run: (total_syscalls, process_count).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count { total: u64, procs: u64 }

/// Run `bin argv...` and parse the sysctr report line. `ptrace_style` adds the
/// `--` separator the ptrace CommonToolArguments parser expects before the
/// guest command; kvm/dbi take the guest path positionally.
fn run_sysctr(bin: &Path, guest: &Path, ptrace_style: bool) -> Result<Count, String> {
    let mut cmd = Command::new(bin);
    if ptrace_style {
        cmd.arg("--");
    }
    cmd.arg(guest);
    let out = cmd.output().map_err(|e| format!("spawn {bin:?}: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    parse_report(&text).ok_or_else(|| format!("no sysctr report in output of {bin:?}:\n{text}"))
}

/// Parse ` [reverie-sysctr] Total syscalls in process tree: N across M process(es).`
fn parse_report(text: &str) -> Option<Count> {
    let line = text.lines().find(|l| l.contains("Total syscalls in process tree:"))?;
    let after = line.split("process tree:").nth(1)?;
    let total: u64 = after.trim().split_whitespace().next()?.parse().ok()?;
    let across = after.split("across").nth(1)?;
    let procs: u64 = across.trim().split_whitespace().next()?.parse().ok()?;
    Some(Count { total, procs })
}

/// Run `reps` times; return all observations (for stability checks).
fn run_many(bin: &Path, guest: &Path, ptrace_style: bool, reps: u32) -> Result<Vec<Count>, String> {
    (0..reps).map(|_| run_sysctr(bin, guest, ptrace_style)).collect()
}

// --------------------------------------------------------------------- harness

struct Backends { ptrace: Option<PathBuf>, kvm: Option<PathBuf>, dbi: Option<PathBuf> }

fn find_bin_dir(explicit: Option<String>) -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(d) = explicit { cands.push(PathBuf::from(d)); }
    if let Ok(d) = std::env::var("REVERIE_SYSCTR_BINDIR") { cands.push(PathBuf::from(d)); }
    // usual cargo output dirs relative to a reverie checkout
    for rel in ["target/debug", "target/release", "../target/debug", "../../target/debug"] {
        cands.push(PathBuf::from(rel));
    }
    cands.into_iter().find(|d| d.join("reverie-sysctr-ptrace").exists())
}

fn detect(bin_dir: &Path) -> Backends {
    let pick = |name: &str| {
        let p = bin_dir.join(name);
        if p.exists() { Some(p) } else { None }
    };
    let kvm = pick("reverie-sysctr-kvm").filter(|_| Path::new("/dev/kvm").exists());
    let dbi = pick("reverie-sysctr-dbi").filter(|_| {
        std::env::var("DYNAMORIO_HOME").is_ok() && std::env::var("REVERIE_DBI_CLIENT").is_ok()
    });
    Backends { ptrace: pick("reverie-sysctr-ptrace"), kvm, dbi }
}

/// A single pass/fail line in the report.
struct Check { name: String, pass: bool, detail: String }

fn main() {
    // -------- args
    let args: Vec<String> = std::env::args().collect();
    let mut bin_dir_arg = None;
    let mut reps: u32 = 5;
    let mut keep = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bin-dir" => { i += 1; bin_dir_arg = args.get(i).cloned(); }
            "--repeats" => { i += 1; reps = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(5); }
            "--keep" => keep = true,
            "-h" | "--help" => { eprintln!("usage: validate-cross-backend.rs [--bin-dir DIR] [--repeats N] [--keep]"); return; }
            other => { eprintln!("unknown arg: {other}"); std::process::exit(2); }
        }
        i += 1;
    }

    let bin_dir = match find_bin_dir(bin_dir_arg) {
        Some(d) => d,
        None => {
            eprintln!("FATAL: could not find reverie-sysctr-ptrace. Pass --bin-dir or set REVERIE_SYSCTR_BINDIR.");
            std::process::exit(2);
        }
    };
    let be = detect(&bin_dir);
    let ptrace = match &be.ptrace {
        Some(p) => p.clone(),
        None => { eprintln!("FATAL: reverie-sysctr-ptrace missing in {bin_dir:?}"); std::process::exit(2); }
    };

    // -------- temp guests
    let tmp = std::env::temp_dir().join(format!("sysctr-xbackend-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    println!("cross-backend syscall-counter validation");
    println!("  bin-dir : {}", bin_dir.display());
    println!("  ptrace  : yes");
    println!("  kvm     : {}", if be.kvm.is_some() { "yes (/dev/kvm present)" } else { "no (binary and/or /dev/kvm absent)" });
    println!("  dbi     : {}", if be.dbi.is_some() { "yes" } else { "no (needs per-tool client + DYNAMORIO_HOME + REVERIE_DBI_CLIENT)" });
    println!("  repeats : {reps}");
    println!();

    let mut checks: Vec<Check> = Vec::new();
    let stable = |v: &[Count]| v.iter().all(|c| *c == v[0]);

    // ===== Test 1: single-process cross-backend agreement (hard gate) =====
    println!("[1] single-process agreement (guest = N getpid + exit_group)");
    for n in [3u32, 5, 10] {
        let guest = write_guest(&tmp, &format!("single_{n}"), &guest_single(n));
        let expect_guest = (n + 1) as u64; // N getpid + exit_group
        let p = match run_many(&ptrace, &guest, true, reps) {
            Ok(v) => v, Err(e) => { checks.push(Check { name: format!("single N={n} ptrace"), pass: false, detail: e }); continue; }
        };
        let p_stable = stable(&p);
        // ptrace observes the launch execve: total = guest + 1.
        let p_ok = p_stable && p[0].procs == 1 && p[0].total == expect_guest + 1;
        let mut detail = format!("ptrace total={} procs={} (expect {}=guest+execve){}",
            p[0].total, p[0].procs, expect_guest + 1, if p_stable { "" } else { " UNSTABLE" });

        let mut pass = p_ok;
        if let Some(kvm) = &be.kvm {
            match run_many(kvm, &guest, false, reps) {
                Ok(k) => {
                    let k_stable = stable(&k);
                    // KVM boots at entry: no launch execve, so it counts exactly the guest syscalls.
                    let k_ok = k_stable && k[0].procs == 1 && k[0].total == expect_guest;
                    let agree = p[0].total == k[0].total + 1;
                    detail += &format!("; kvm total={} (expect {}=guest){}; ptrace==kvm+1: {}",
                        k[0].total, expect_guest, if k_stable { "" } else { " UNSTABLE" }, agree);
                    pass = pass && k_ok && agree;
                }
                Err(e) => { detail += &format!("; kvm ERROR: {e}"); pass = false; }
            }
        } else {
            detail += "; kvm skipped";
        }
        checks.push(Check { name: format!("single N={n}"), pass, detail });
    }

    // ===== Test 2: fork-tree aggregation on ptrace (hard gate = process_count) =====
    println!("[2] fork-tree aggregation, ptrace (parent + K children, each {CHILD_GETPIDS} getpid)");
    let m = CHILD_GETPIDS;
    for k in [1i32, 2, 4] {
        let guest = write_guest(&tmp, &format!("fork_{k}"), &guest_fork(k, m));
        // analytic guest syscalls + launch execve
        let analytic = (k * m + 3 * k + 1 + 1) as u64;
        let expect_procs = (k + 1) as u64;
        match run_many(&ptrace, &guest, true, reps) {
            Ok(v) => {
                let procs_ok = v.iter().all(|c| c.procs == expect_procs);
                let totals: Vec<u64> = v.iter().map(|c| c.total).collect();
                let (mn, mx) = (*totals.iter().min().unwrap(), *totals.iter().max().unwrap());
                // wait4/SIGCHLD restart races only ADD syscalls; tolerate a small positive wobble.
                let tol = (2 * expect_procs).max(4);
                let total_ok = mn >= analytic && mx <= analytic + tol;
                let pass = procs_ok; // hard gate = aggregation topology
                let detail = format!(
                    "procs={} (expect {}) [{}]; total range [{}..{}] analytic {} (soft +/-restart tol {}: {})",
                    v[0].procs, expect_procs, if procs_ok { "OK" } else { "MISMATCH" },
                    mn, mx, analytic, tol, if total_ok { "within" } else { "OUT-OF-RANGE" });
                checks.push(Check { name: format!("fork K={k} ptrace aggregation"), pass, detail });
            }
            Err(e) => checks.push(Check { name: format!("fork K={k} ptrace"), pass: false, detail: e }),
        }
    }

    // ===== Test 3: KVM fork behaviour (informational) =====
    if let Some(kvm) = &be.kvm {
        let guest = write_guest(&tmp, "fork_kvm", &guest_fork(3, m));
        match run_sysctr(kvm, &guest, false) {
            Ok(c) => println!(
                "[3] kvm fork guest: total={} across {} process(es) -- single-process adapter; \
                 KVM does not surface a fork tree to the tool (informational, not gated)",
                c.total, c.procs),
            Err(e) => println!("[3] kvm fork guest: run error (informational): {e}"),
        }
    } else {
        println!("[3] kvm fork guest: skipped (kvm unavailable)");
    }

    // ===== Test 4: DBI (blocked, documented) =====
    if be.dbi.is_some() {
        println!("[4] dbi: client provided; NOTE global state still fragments across fork until \
                  impl-dbi-global-state-fix lands -- fork-tree counts will be per-process, not aggregated");
    } else {
        println!("[4] dbi: SKIPPED -- no per-tool native client (sysctr not baked into a DynamoRIO \
                  client) and/or DYNAMORIO_HOME/REVERIE_DBI_CLIENT unset. Even with a client, DBI \
                  global state fragments across fork until impl-dbi-global-state-fix lands.");
    }

    // -------- report
    println!("\n---- results ----");
    let mut failed = 0;
    for c in &checks {
        println!("  [{}] {}: {}", if c.pass { "PASS" } else { "FAIL" }, c.name, c.detail);
        if !c.pass { failed += 1; }
    }
    println!("\n{}/{} hard checks passed", checks.len() - failed, checks.len());

    if !keep { let _ = std::fs::remove_dir_all(&tmp); } else { println!("guests kept in {}", tmp.display()); }
    if failed > 0 { std::process::exit(1); }
}
