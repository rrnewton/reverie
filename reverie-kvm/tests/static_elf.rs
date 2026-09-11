/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![cfg(target_arch = "x86_64")]

use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Barrier;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use kvm_ioctls::Kvm;
use reverie::BackendChildWaitEvent;
use reverie::BackendChildWaitState;
use reverie::BackendStatsRequest;
use reverie::BackendStatsSource;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Stack;
use reverie::Subscription;
use reverie::ThreadOwnership;
use reverie::Tool;
use reverie::syscalls::CArrayPtr;
use reverie::syscalls::CStrPtr;
use reverie::syscalls::Errno;
use reverie::syscalls::Execve;
use reverie::syscalls::ExitGroup;
use reverie::syscalls::Fork;
use reverie::syscalls::FromToRaw;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::PathPtr;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;
use reverie_kvm::CounterTool;
use reverie_kvm::Error;
use reverie_kvm::HierarchicalCounterTool;
use reverie_kvm::HierarchicalTotals;
use reverie_kvm::KvmBackend;
use reverie_kvm::KvmBackendStats;
use reverie_kvm::KvmExitReason;
use reverie_kvm::StraceTool;

const MEMORY_SIZE: usize = 16 * 1024 * 1024;

#[test]
fn initial_exec_binding_bytes_different_file() {
    initial_exec_binding_control(
        "initial_exec_binding_bytes_different_file",
        false,
        "different",
    );
}

#[test]
fn initial_exec_binding_bytes_identical_file() {
    initial_exec_binding_control(
        "initial_exec_binding_bytes_identical_file",
        false,
        "identical",
    );
}

#[test]
fn initial_exec_binding_bytes_missing_file() {
    initial_exec_binding_control("initial_exec_binding_bytes_missing_file", false, "missing");
}

#[test]
fn initial_exec_binding_bytes_arbitrary_argv() {
    initial_exec_binding_control(
        "initial_exec_binding_bytes_arbitrary_argv",
        false,
        "arbitrary",
    );
}

#[test]
fn initial_exec_binding_bytes_replace_known_binding() {
    initial_exec_binding_control(
        "initial_exec_binding_bytes_replace_known_binding",
        false,
        "after-file",
    );
}

#[test]
fn initial_exec_binding_file_different_file() {
    initial_exec_binding_control(
        "initial_exec_binding_file_different_file",
        true,
        "different",
    );
}

#[test]
fn initial_exec_binding_file_identical_file() {
    initial_exec_binding_control(
        "initial_exec_binding_file_identical_file",
        true,
        "identical",
    );
}

#[test]
fn initial_exec_binding_file_arbitrary_argv() {
    initial_exec_binding_control(
        "initial_exec_binding_file_arbitrary_argv",
        true,
        "arbitrary",
    );
}

#[test]
fn initial_exec_binding_file_unlinked_before_install() {
    initial_exec_binding_control(
        "initial_exec_binding_file_unlinked_before_install",
        true,
        "unlink",
    );
}

#[test]
fn initial_exec_binding_file_replaced_before_install() {
    initial_exec_binding_control(
        "initial_exec_binding_file_replaced_before_install",
        true,
        "replace",
    );
}

#[test]
fn initial_exec_binding_file_failed_read_preserves_image() {
    initial_exec_binding_control(
        "initial_exec_binding_file_failed_read_preserves_image",
        true,
        "read-error",
    );
}

fn initial_exec_binding_control(test: &str, file_backed: bool, case: &str) {
    use std::io::Seek;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;

    if !leader_self_exec_bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let program = compile_c_program(&directory.0, "image-a", INITIAL_EXEC_BINDING_PROGRAM);
    let other = compile_c_program(&directory.0, "image-b", "int main(void) { return 77; }");
    if case == "identical" {
        std::fs::copy(&program, &other).unwrap();
        assert_eq!(
            std::fs::read(&program).unwrap(),
            std::fs::read(&other).unwrap()
        );
    }
    let file = std::fs::File::open(&program).unwrap();
    let metadata = file.metadata().unwrap();
    assert_ne!(metadata.ino(), std::fs::metadata(&other).unwrap().ino());
    let image = std::fs::read(&program).unwrap();
    let missing = directory.0.join("absent-image");
    let argv0 = match case {
        "missing" => missing.to_str().unwrap(),
        "arbitrary" => "arbitrary-argv0-not-a-path",
        _ => other.to_str().unwrap(),
    };
    let deleted = matches!(case, "unlink" | "replace");
    if case == "unlink" {
        std::fs::remove_file(&program).unwrap();
    } else if case == "replace" {
        std::fs::rename(&other, &program).unwrap();
    }
    let environment = [
        format!("BINDING_KNOWN={}", u8::from(file_backed)),
        format!("BINDING_DEV={}", metadata.dev()),
        format!("BINDING_INO={}", metadata.ino()),
        format!("BINDING_ARGV={argv0}"),
        format!(
            "BINDING_LINK={}{}",
            program.display(),
            if deleted { " (deleted)" } else { "" }
        ),
    ];
    let envp = environment.iter().map(String::as_str).collect::<Vec<_>>();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    if file_backed {
        let mut offset_alias = file.try_clone().unwrap();
        offset_alias.seek(std::io::SeekFrom::Start(7)).unwrap();
        backend
            .install_static_elf_file_with_context(
                file,
                &[argv0, "argument-preserved"],
                &envp,
                &directory.0,
            )
            .unwrap();
        assert_eq!(offset_alias.stream_position().unwrap(), 7);
        if case == "read-error" {
            let unreadable = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH)
                .open(&other)
                .unwrap();
            let error = backend
                .install_static_elf_file_with_context(
                    unreadable,
                    &["wrong-image"],
                    &[],
                    &directory.0,
                )
                .unwrap_err();
            assert!(
                matches!(error, Error::HostIo(ref error) if error.raw_os_error() == Some(libc::EBADF)),
                "{error}"
            );
        }
    } else {
        if case == "after-file" {
            backend
                .install_static_elf_file_with_context(
                    file.try_clone().unwrap(),
                    &[argv0, "argument-preserved"],
                    &envp,
                    &directory.0,
                )
                .unwrap();
        }
        backend
            .install_static_elf_with_context(
                &image,
                &[argv0, "argument-preserved"],
                &envp,
                &directory.0,
            )
            .unwrap();
        drop(file);
    }
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(
        code,
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        stdout,
        if file_backed {
            b"known object and selfexec exact=PASS\n".as_slice()
        } else {
            b"unknown backing and failed selfexec exact=PASS\n".as_slice()
        }
    );
    assert!(
        stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&stderr)
    );
}

const INITIAL_EXEC_BINDING_PROGRAM: &str = r###"
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <unistd.h>

extern char **environ;

int main(int argc, char **argv) {
    if (argc != 2 || strcmp(argv[0], getenv("BINDING_ARGV")) || strcmp(argv[1], "argument-preserved")) return 10;
    int known = !strcmp(getenv("BINDING_KNOWN"), "1");
    unsigned char actual[4096], expected[4096];
    memset(actual, 0x5a, sizeof(actual)); memset(expected, 0x5a, sizeof(expected));
    errno = 0;
    int result = stat("/proc/self/exe", (struct stat *)actual);
    int error = errno;
    if (known) {
        struct stat metadata;
        memcpy(&metadata, actual, sizeof(metadata));
        if (result || error || metadata.st_dev != strtoull(getenv("BINDING_DEV"), NULL, 10) || metadata.st_ino != strtoull(getenv("BINDING_INO"), NULL, 10) || memcmp(actual + sizeof(metadata), expected + sizeof(metadata), sizeof(actual) - sizeof(metadata))) {
            printf("known stat result=%d errno=%d inode=%llu expected=%s\n", result, error, (unsigned long long)metadata.st_ino, getenv("BINDING_INO"));
            return 11;
        }
        memset(actual, 0x5a, sizeof(actual));
        const char *link = getenv("BINDING_LINK");
        size_t length = strlen(link);
        if (length >= sizeof(expected)) return 12;
        memcpy(expected, link, length);
        if (readlink("/proc/self/exe", (char *)actual, sizeof(actual)) != (ssize_t)length || memcmp(actual, expected, sizeof(actual))) return 13;
    } else if (result != -1 || error != ENOENT || memcmp(actual, expected, sizeof(actual))) {
        printf("unknown stat result=%d errno=%d full4096_unchanged=%d\n", result, error, !memcmp(actual, expected, sizeof(actual)));
        return 14;
    }
    unsigned char name[64], expected_name[64];
    memset(name, 0x5a, sizeof(name)); memset(expected_name, 0x5a, sizeof(expected_name));
    if (getenv("BINDING_AFTER")) {
        memset(expected_name, 0, 16); memcpy(expected_name, "exe", 3);
        if (prctl(PR_GET_NAME, name) || memcmp(name, expected_name, sizeof(name))) return 15;
        puts("known object and selfexec exact=PASS");
        return 0;
    }
    if (prctl(PR_SET_NAME, "binding-before") || setenv("BINDING_AFTER", "1", 1)) return 16;
    errno = 0;
    execve("/proc/self/exe", argv, environ);
    error = errno;
    memset(expected_name, 0, 16); memcpy(expected_name, "binding-before", 14);
    if (known || error != ENOENT || prctl(PR_GET_NAME, name) || memcmp(name, expected_name, sizeof(name))) return 17;
    puts("unknown backing and failed selfexec exact=PASS");
    return 0;
}
"###;

#[test]
fn leader_self_exec_output_permissions() {
    if !leader_self_exec_bounded("leader_self_exec_output_permissions") {
        return;
    }
    let directory = TestDirectory::new();
    let program = compile_c_program(
        &directory.0,
        "running-image",
        LEADER_SELF_EXEC_OUTPUT_PROGRAM,
    );
    leader_self_exec_guest(
        &directory.0,
        &program,
        &[],
        b"stat/readlink RW/RO/NONE exact errno and full4096=PASS\n",
    );
}

const LEADER_SELF_EXEC_OUTPUT_PROGRAM: &str = r###"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 1 || strlen(argv[0]) >= 4096) return 10;
    int failures = 0;
    for (int operation = 0; operation != 2; ++operation) {
        for (int access = 0; access != 3; ++access) {
            unsigned char *actual = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
            unsigned char expected[4096];
            if (actual == MAP_FAILED) return 11;
            memset(actual, 0x5a, 4096); memset(expected, 0x5a, sizeof(expected));
            if (!access) {
                if (operation) memcpy(expected, argv[0], strlen(argv[0]));
                else if (syscall(SYS_newfstatat, AT_FDCWD, argv[0], expected, 0)) return 12;
            }
            int protection = access == 0 ? PROT_READ | PROT_WRITE : access == 1 ? PROT_READ : PROT_NONE;
            if (mprotect(actual, 4096, protection)) return 13;
            errno = 0;
            long result = operation ? syscall(SYS_readlink, "/proc/self/exe", actual, 4096) : syscall(SYS_newfstatat, AT_FDCWD, "/proc/self/exe", actual, 0);
            int error = errno;
            if (mprotect(actual, 4096, PROT_READ | PROT_WRITE)) return 14;
            long expected_result = access ? -1 : operation ? (long)strlen(argv[0]) : 0;
            int expected_error = access ? EFAULT : 0;
            int equal = memcmp(actual, expected, sizeof(expected)) == 0;
            if (result != expected_result || error != expected_error || !equal) {
                printf("operation=%d access=%d result=%ld errno=%d full4096=%d expected_result=%ld expected_errno=%d\n", operation, access, result, error, equal, expected_result, expected_error);
                ++failures;
            }
            if (munmap(actual, 4096)) return 15;
        }
    }
    if (!failures) puts("stat/readlink RW/RO/NONE exact errno and full4096=PASS");
    return failures ? 1 : 0;
}
"###;

fn leader_self_exec_lifetime_control(test: &str, mode: &str, method: &str) {
    if !leader_self_exec_bounded(test) {
        return;
    }
    for with_tool in [false, true] {
        let directory = TestDirectory::new();
        let program = compile_c_program(
            &directory.0,
            "running-image",
            LEADER_SELF_EXEC_LIFETIME_PROGRAM,
        );
        let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
        backend
            .install_static_elf_file_with_context(
                std::fs::File::open(&program).unwrap(),
                &[program.to_str().unwrap(), mode, method],
                &[],
                &directory.0,
            )
            .unwrap();
        let (code, stdout, stderr) = if with_tool {
            let (_, code, stdout, stderr) = futures::executor::block_on(
                backend.run_static_elf_with_tool::<StraceTool>((), true),
            )
            .unwrap();
            (code, stdout, stderr)
        } else {
            backend.run_static_elf_captured().unwrap()
        };
        let expected: &[u8] = if mode == "fork" {
            b"child-exec\nparent-after-child\n"
        } else {
            b"leader-after-thread-exec\n"
        };
        assert_eq!(
            code,
            0,
            "mode={mode} method={method} tool={with_tool} stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(
            stdout, expected,
            "mode={mode} method={method} tool={with_tool}"
        );
        assert!(
            stderr.is_empty(),
            "mode={mode} method={method} tool={with_tool} stderr={}",
            String::from_utf8_lossy(&stderr)
        );
    }
}

#[test]
fn leader_self_exec_lifetime_fork_0() {
    leader_self_exec_lifetime_control("leader_self_exec_lifetime_fork_0", "fork", "0");
}

#[test]
fn leader_self_exec_lifetime_fork_1() {
    leader_self_exec_lifetime_control("leader_self_exec_lifetime_fork_1", "fork", "1");
}

#[test]
fn leader_self_exec_lifetime_thread_0() {
    leader_self_exec_lifetime_control("leader_self_exec_lifetime_thread_0", "thread", "0");
}

#[test]
fn leader_self_exec_lifetime_thread_1() {
    leader_self_exec_lifetime_control("leader_self_exec_lifetime_thread_1", "thread", "1");
}

const LEADER_SELF_EXEC_LIFETIME_PROGRAM: &str = r###"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile unsigned image_value = 0x11223344;
static int ready_pipe[2], stop_pipe[2];
static long old_tid;
static int name_is(const char *name) {
    unsigned char actual[64], expected[64];
    memset(actual, 0x5a, sizeof(actual));
    memset(expected, 0x5a, sizeof(expected));
    memset(expected, 0, 16);
    memcpy(expected, name, strlen(name));
    return prctl(PR_GET_NAME, actual) == 0 && !memcmp(actual, expected, sizeof(actual));
}
static void *sibling(void *unused) {
    (void)unused;
    old_tid = syscall(SYS_gettid);
    if (write(ready_pipe[1], "R", 1) != 1) return (void *)1;
    char stop;
    return read(stop_pipe[0], &stop, 1) == 1 ? NULL : (void *)2;
}
int main(int argc, char **argv) {
    const char *stage = getenv("LIFETIME_STAGE");
    if (stage) {
        if (argc != 2 || strcmp(argv[0], "different-argv-zero") || strcmp(argv[1], "after")) return 30;
        if (image_value != 0x11223344 || !name_is("exe")) return 31;
        if (getpid() != strtol(getenv("SAVED_PID"), NULL, 10) || getppid() != strtol(getenv("SAVED_PPID"), NULL, 10)) return 32;
        struct stat actual;
        if (stat("/proc/self/exe", &actual) || actual.st_dev != strtoull(getenv("SAVED_DEV"), NULL, 10) || actual.st_ino != strtoull(getenv("SAVED_INO"), NULL, 10)) return 33;
        unsigned char link[4096], expected[4096];
        memset(link, 0x5a, sizeof(link)); memset(expected, 0x5a, sizeof(expected));
        const char *path = getenv("SAVED_LINK");
        size_t length = strlen(path);
        if (length >= sizeof(expected)) return 34;
        memcpy(expected, path, length);
        if (readlink("/proc/self/exe", (char *)link, sizeof(link)) != (ssize_t)length || memcmp(link, expected, sizeof(link))) return 35;
        if (prctl(PR_SET_NAME, "changed-after") || !name_is("changed-after")) return 36;
        if (!strcmp(stage, "thread")) {
            errno = 0;
            if (syscall(SYS_tgkill, getpid(), strtol(getenv("OLD_TID"), NULL, 10), 0) != -1 || errno != ESRCH) return 38;
            return write(1, "leader-after-thread-exec\n", 25) == 25 ? 0 : 39;
        }
        return write(1, "child-exec\n", 11) == 11 ? 37 : 40;
    }
    if (argc != 3 || prctl(PR_SET_NAME, "parent-before") || !name_is("parent-before")) return 10;
    int thread_case = !strcmp(argv[1], "thread");
    struct stat original;
    if (stat("/proc/self/exe", &original)) return 11;
    image_value = 0x88776655;
    pthread_t worker;
    if (thread_case) {
        if (pipe(ready_pipe) || pipe(stop_pipe) || pthread_create(&worker, NULL, sibling, NULL)) return 12;
        unsigned char ready[16], expected[16];
        memset(ready, 0x5a, sizeof(ready)); memset(expected, 0x5a, sizeof(expected)); expected[0] = 'R';
        if (read(ready_pipe[0], ready, 1) != 1 || memcmp(ready, expected, sizeof(ready))) return 13;
    } else {
        pid_t child = fork();
        if (child < 0) return 14;
        if (child) {
            int status = 0;
            if (waitpid(child, &status, 0) != child || status != (37 << 8)) return 15;
            if (image_value != 0x88776655 || !name_is("parent-before")) return 16;
            errno = 0;
            if (access(argv[0], F_OK) != -1 || errno != ENOENT) return 17;
            return write(1, "parent-after-child\n", 19) == 19 ? 0 : 18;
        }
        if (unlink(argv[0])) return 19;
    }
    char setting[64], pid[64], parent[64], tid[64], device[64], inode[64], link[4200];
    snprintf(setting, sizeof(setting), "LIFETIME_STAGE=%s", argv[1]);
    snprintf(pid, sizeof(pid), "SAVED_PID=%d", getpid());
    snprintf(parent, sizeof(parent), "SAVED_PPID=%d", getppid());
    snprintf(tid, sizeof(tid), "OLD_TID=%ld", old_tid);
    snprintf(device, sizeof(device), "SAVED_DEV=%llu", (unsigned long long)original.st_dev);
    snprintf(inode, sizeof(inode), "SAVED_INO=%llu", (unsigned long long)original.st_ino);
    if (snprintf(link, sizeof(link), "SAVED_LINK=%s%s", argv[0], thread_case ? "" : " (deleted)") >= (int)sizeof(link)) return 20;
    char *environment[] = {setting, pid, parent, tid, device, inode, link, NULL};
    char *arguments[] = {"different-argv-zero", "after", NULL};
    if (atoi(argv[2])) syscall(SYS_execveat, AT_FDCWD, "/proc/self/exe", arguments, environment, 0);
    else syscall(SYS_execve, "/proc/self/exe", arguments, environment);
    int error = errno;
    if (thread_case) {
        void *result;
        if (write(stop_pipe[1], "S", 1) != 1 || pthread_join(worker, &result) || result) return 21;
    }
    printf("exec returned errno=%d\n", error);
    return 22;
}
"###;

fn compile_assembly_program(directory: &std::path::Path, name: &str, source: &str) -> PathBuf {
    let source_path = directory.join(format!("{name}.S"));
    let executable_path = directory.join(name);
    std::fs::write(&source_path, source).unwrap();
    let output = std::process::Command::new("/usr/bin/gcc")
        .args(["-nostdlib", "-static", "-Wl,--build-id=none"])
        .arg(&source_path)
        .arg("-o")
        .arg(&executable_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "gcc failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    executable_path
}

#[test]
fn self_exec_proc_aliases_use_loaded_image_after_unlink_or_replacement() {
    if !leader_self_exec_bounded(
        "self_exec_proc_aliases_use_loaded_image_after_unlink_or_replacement",
    ) {
        return;
    }

    const ROOT_PID: i32 = 37;
    const ARGV0: &str = "preserved-argv0";
    const ARGV1: &str = "after-exec";
    const ENVP0: &str = "SELF_EXEC_ENV=preserved";
    const IMAGE_MARKER: &str = "same-image\n";

    for (case, (name, path, execveat)) in [
        ("self-exec-proc-self-execve", "/proc/self/exe", false),
        ("self-exec-proc-pid-execve", "/proc/37/exe", false),
        (
            "self-exec-proc-thread-self-execve",
            "/proc/thread-self/exe",
            false,
        ),
        (
            "self-exec-proc-self-task-execve",
            "/proc/self/task/37/exe",
            false,
        ),
        (
            "self-exec-proc-tgid-task-execve",
            "/proc/37/task/37/exe",
            false,
        ),
        ("self-exec-proc-self-execveat", "/proc/self/exe", true),
        ("self-exec-proc-pid-execveat", "/proc/37/exe", true),
    ]
    .into_iter()
    .enumerate()
    {
        let exec = if execveat {
            // execveat(AT_FDCWD, path, argv, envp, 0)
            r#"
                mov %rdx, %r10
                mov %rsi, %rdx
                mov %rdi, %rsi
                mov $-100, %rdi
                xor %r8d, %r8d
                mov $322, %eax
                syscall
            "#
        } else {
            // execve(path, argv, envp)
            r#"
                mov $59, %eax
                syscall
            "#
        };
        let source = format!(
            r#"
                .global _start
                .text
            _start:
                cmpq $2, (%rsp)
                je after_exec

                lea self_path(%rip), %rdi
                lea replacement_argv(%rip), %rsi
                lea replacement_envp(%rip), %rdx
                {exec}
                neg %eax
                mov %eax, %edi
                mov $231, %eax
                syscall

            after_exec:
                lea self_path(%rip), %rdi
                lea link_buffer(%rip), %rsi
                mov $4096, %edx
                mov $89, %eax
                syscall
                test %rax, %rax
                js exit_with_errno
                mov %eax, %edx
                mov $1, %edi
                lea link_buffer(%rip), %rsi
                mov $1, %eax
                syscall
                call write_newline

                mov $1, %edi
                lea image_marker(%rip), %rsi
                mov ${image_marker_len}, %edx
                mov $1, %eax
                syscall

                mov $1, %edi
                mov 8(%rsp), %rsi
                mov ${argv0_len}, %edx
                mov $1, %eax
                syscall
                call write_newline

                mov $1, %edi
                mov 16(%rsp), %rsi
                mov ${argv1_len}, %edx
                mov $1, %eax
                syscall
                call write_newline

                mov $1, %edi
                mov 32(%rsp), %rsi
                mov ${envp0_len}, %edx
                mov $1, %eax
                syscall
                call write_newline

                xor %edi, %edi
                mov $231, %eax
                syscall

            exit_with_errno:
                neg %eax
                mov %eax, %edi
                mov $231, %eax
                syscall

            write_newline:
                mov $1, %edi
                lea newline(%rip), %rsi
                mov $1, %edx
                mov $1, %eax
                syscall
                ret

                .section .rodata
            self_path:
                .asciz "{path}"
            replacement_argv0:
                .asciz "{argv0}"
            replacement_argv1:
                .asciz "{argv1}"
            replacement_envp0:
                .asciz "{envp0}"
            image_marker:
                .ascii "same-image\n"
            newline:
                .ascii "\n"

                .section .data
                .align 8
            replacement_argv:
                .quad replacement_argv0, replacement_argv1, 0
            replacement_envp:
                .quad replacement_envp0, 0

                .section .bss
                .align 8
            link_buffer:
                .skip 4096
            "#,
            image_marker_len = IMAGE_MARKER.len(),
            argv0_len = ARGV0.len(),
            argv1_len = ARGV1.len(),
            envp0_len = ENVP0.len(),
            argv0 = ARGV0,
            argv1 = ARGV1,
            envp0 = ENVP0,
        );

        let root = TestDirectory::new();
        let executable = compile_assembly_program(&root.0, name, &source);
        let executable_name = executable.to_str().unwrap().to_owned();
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend.set_root_pid(ROOT_PID).unwrap();
        backend
            .install_static_elf_file_with_context(
                std::fs::File::open(&executable).unwrap(),
                &[executable_name.as_str()],
                &["INITIAL=1"],
                &root.0,
            )
            .unwrap();
        let mutation = if case.is_multiple_of(2) {
            std::fs::remove_file(&executable).unwrap();
            "unlinked"
        } else {
            let replacement = root.0.join(format!("{name}-replacement"));
            std::fs::write(
                &replacement,
                static_elf(&[
                    0xbf, 0x63, 0x00, 0x00, 0x00, // mov edi, 99
                    0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
                    0x0f, 0x05, // syscall
                    0x0f, 0x0b, // ud2
                ]),
            )
            .unwrap();
            std::fs::rename(replacement, &executable).unwrap();
            "replaced"
        };

        let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
        let expected = format!(
            "{} (deleted)\n{IMAGE_MARKER}{ARGV0}\n{ARGV1}\n{ENVP0}\n",
            executable.display(),
        );
        assert_eq!(code, 0, "path={path} execveat={execveat} {mutation}");
        assert_eq!(
            stdout,
            expected.as_bytes(),
            "path={path} execveat={execveat} {mutation}"
        );
        assert!(
            stderr.is_empty(),
            "path={path} execveat={execveat} {mutation}"
        );
    }
}

#[test]
fn exec_through_symlink_reports_the_opened_executable_and_preserves_argv0() {
    if !leader_self_exec_bounded(
        "exec_through_symlink_reports_the_opened_executable_and_preserves_argv0",
    ) {
        return;
    }

    const ARGV0: &str = "not-the-path";
    let root = TestDirectory::new();
    let target_source = format!(
        r#"
            .global _start
            .text
        _start:
            lea self_path(%rip), %rdi
            lea link_buffer(%rip), %rsi
            mov $4096, %edx
            mov $89, %eax
            syscall
            test %rax, %rax
            js exit_with_errno

            mov %eax, %edx
            mov $1, %edi
            lea link_buffer(%rip), %rsi
            mov $1, %eax
            syscall
            call write_newline

            mov $1, %edi
            mov 8(%rsp), %rsi
            mov ${argv0_len}, %edx
            mov $1, %eax
            syscall
            call write_newline

            xor %edi, %edi
            mov $231, %eax
            syscall

        write_newline:
            mov $1, %edi
            lea newline(%rip), %rsi
            mov $1, %edx
            mov $1, %eax
            syscall
            ret

        exit_with_errno:
            neg %eax
            mov %eax, %edi
            mov $231, %eax
            syscall

            .section .rodata
        self_path:
            .asciz "/proc/self/exe"
        newline:
            .ascii "\n"

            .section .bss
            .align 8
        link_buffer:
            .skip 4096
        "#,
        argv0_len = ARGV0.len(),
    );
    let target = compile_assembly_program(&root.0, "actual-target", &target_source);
    std::fs::create_dir(root.0.join("components")).unwrap();
    let alias = root.0.join("exec-alias");
    std::os::unix::fs::symlink("components/../actual-target", &alias).unwrap();

    let root_source = format!(
        r#"
            .global _start
            .text
        _start:
            lea exec_path(%rip), %rdi
            lea replacement_argv(%rip), %rsi
            xor %edx, %edx
            mov $59, %eax
            syscall
            neg %eax
            mov %eax, %edi
            mov $231, %eax
            syscall

            .section .rodata
        exec_path:
            .asciz "{alias}"
        replacement_argv0:
            .asciz "{argv0}"

            .section .data
            .align 8
        replacement_argv:
            .quad replacement_argv0, 0
        "#,
        alias = alias.display(),
        argv0 = ARGV0,
    );
    let launcher = compile_assembly_program(&root.0, "symlink-exec-launcher", &root_source);
    let launcher = launcher.to_str().unwrap();
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(launcher).unwrap(),
            &[launcher],
            &[],
            &root.0,
        )
        .unwrap();

    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    let expected = format!("{}\n{ARGV0}\n", target.canonicalize().unwrap().display());
    assert_eq!(code, 0);
    assert_eq!(stdout, expected.as_bytes());
    assert!(stderr.is_empty());
}

#[test]
fn repeated_self_exec_preserves_executable_identity_argv_and_envp() {
    if !leader_self_exec_bounded("repeated_self_exec_preserves_executable_identity_argv_and_envp") {
        return;
    }

    const ROOT_PID: i32 = 37;
    const FIRST_ARGV0: &str = "first-non-path-argv0";
    const FIRST_ARGV1: &str = "stage-one-argument";
    const FIRST_ENVP0: &str = "FIRST_ENV=preserved";
    const SECOND_ARGV0: &str = "second-non-path-argv0";
    const SECOND_ARGV1: &str = "stage-two-argument";
    const SECOND_ARGV2: &str = "final-argument";
    const SECOND_ENVP0: &str = "SECOND_ENV=preserved";
    const STAGE_ONE_MARKER: &str = "stage-one\n";
    const STAGE_TWO_MARKER: &str = "stage-two\n";

    let source = r#"
        .global _start
        .text
    _start:
        cmpq $1, (%rsp)
        je initial_exec
        cmpq $2, (%rsp)
        je after_first_exec
        cmpq $3, (%rsp)
        je after_second_exec
        mov $99, %edi
        jmp exit_with_code

    initial_exec:
        lea self_path(%rip), %rdi
        lea first_argv(%rip), %rsi
        lea first_envp(%rip), %rdx
        mov $59, %eax
        syscall
        jmp exit_with_errno

    after_first_exec:
        lea stage_one_marker(%rip), %rsi
        mov $10, %edx
        call write_buffer
        lea self_path(%rip), %rdi
        call write_link
        lea numeric_path(%rip), %rdi
        call write_link

        mov 8(%rsp), %rsi
        mov $20, %edx
        call write_buffer
        call write_newline
        mov 16(%rsp), %rsi
        mov $18, %edx
        call write_buffer
        call write_newline
        mov 32(%rsp), %rsi
        mov $19, %edx
        call write_buffer
        call write_newline

        lea numeric_path(%rip), %rdi
        lea second_argv(%rip), %rsi
        lea second_envp(%rip), %rdx
        mov %rdx, %r10
        mov %rsi, %rdx
        mov %rdi, %rsi
        mov $-100, %rdi
        xor %r8d, %r8d
        mov $322, %eax
        syscall
        jmp exit_with_errno

    after_second_exec:
        lea stage_two_marker(%rip), %rsi
        mov $10, %edx
        call write_buffer
        lea self_path(%rip), %rdi
        call write_link
        lea numeric_path(%rip), %rdi
        call write_link

        mov 8(%rsp), %rsi
        mov $21, %edx
        call write_buffer
        call write_newline
        mov 16(%rsp), %rsi
        mov $18, %edx
        call write_buffer
        call write_newline
        mov 24(%rsp), %rsi
        mov $14, %edx
        call write_buffer
        call write_newline
        mov 40(%rsp), %rsi
        mov $20, %edx
        call write_buffer
        call write_newline
        xor %edi, %edi
        jmp exit_with_code

    write_link:
        lea link_buffer(%rip), %rsi
        mov $4096, %edx
        mov $89, %eax
        syscall
        test %rax, %rax
        js exit_with_errno
        mov %eax, %edx
        lea link_buffer(%rip), %rsi
        call write_buffer
        jmp write_newline

    write_buffer:
        mov $1, %edi
        mov $1, %eax
        syscall
        ret

    write_newline:
        mov $1, %edi
        lea newline(%rip), %rsi
        mov $1, %edx
        mov $1, %eax
        syscall
        ret

    exit_with_errno:
        neg %eax
        mov %eax, %edi
    exit_with_code:
        mov $231, %eax
        syscall

        .section .rodata
    self_path:
        .asciz "/proc/self/exe"
    numeric_path:
        .asciz "/proc/37/exe"
    first_argv0:
        .asciz "first-non-path-argv0"
    first_argv1:
        .asciz "stage-one-argument"
    first_envp0:
        .asciz "FIRST_ENV=preserved"
    second_argv0:
        .asciz "second-non-path-argv0"
    second_argv1:
        .asciz "stage-two-argument"
    second_argv2:
        .asciz "final-argument"
    second_envp0:
        .asciz "SECOND_ENV=preserved"
    stage_one_marker:
        .ascii "stage-one\n"
    stage_two_marker:
        .ascii "stage-two\n"
    newline:
        .ascii "\n"

        .section .data
        .align 8
    first_argv:
        .quad first_argv0, first_argv1, 0
    first_envp:
        .quad first_envp0, 0
    second_argv:
        .quad second_argv0, second_argv1, second_argv2, 0
    second_envp:
        .quad second_envp0, 0

        .section .bss
        .align 8
    link_buffer:
        .skip 4096
    "#;

    let root = TestDirectory::new();
    let executable = compile_assembly_program(&root.0, "repeated-self-exec", source);
    let executable = executable.to_str().unwrap();
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend.set_root_pid(ROOT_PID).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(executable).unwrap(),
            &[executable],
            &["INITIAL=1"],
            &root.0,
        )
        .unwrap();

    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    let expected = format!(
        "{STAGE_ONE_MARKER}{executable}\n{executable}\n{FIRST_ARGV0}\n{FIRST_ARGV1}\n{FIRST_ENVP0}\n\
         {STAGE_TWO_MARKER}{executable}\n{executable}\n{SECOND_ARGV0}\n{SECOND_ARGV1}\n\
         {SECOND_ARGV2}\n{SECOND_ENVP0}\n"
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, expected.as_bytes());
    assert!(stderr.is_empty());
}

#[test]
fn exec_comm_tracks_the_requested_filename_independently_of_exe_target_and_argv0() {
    if !leader_self_exec_bounded(
        "exec_comm_tracks_the_requested_filename_independently_of_exe_target_and_argv0",
    ) {
        return;
    }

    let root = TestDirectory::new();
    let target = compile_c_program(
        &root.0,
        "actual-name",
        r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern char **environ;

static int write_all(const char *bytes, size_t length) {
  while (length != 0) {
    ssize_t written = write(STDOUT_FILENO, bytes, length);
    if (written <= 0) return -1;
    bytes += written;
    length -= (size_t)written;
  }
  return 0;
}

static int print_identity(const char *argv0) {
  char link[4096];
  ssize_t link_length = readlink("/proc/self/exe", link, sizeof(link));
  if (link_length < 0 || write_all(link, (size_t)link_length) != 0 ||
      write_all("\n", 1) != 0) return -1;

  int fd = open("/proc/self/stat", O_RDONLY);
  char stat[4096];
  ssize_t length = fd < 0 ? -1 : read(fd, stat, sizeof(stat) - 1);
  if (fd >= 0) close(fd);
  if (length <= 0) return -1;
  stat[length] = 0;
  char *left = strchr(stat, '(');
  char *right = strrchr(stat, ')');
  if (left == NULL || right == NULL || right <= left ||
      write_all(left + 1, (size_t)(right - left - 1)) != 0 ||
      write_all("\n", 1) != 0 || write_all(argv0, strlen(argv0)) != 0 ||
      write_all("\n", 1) != 0) return -1;
  return 0;
}

int main(int argc, char **argv) {
  if (print_identity(argv[0]) != 0) return 20;
  if (argc == 1) {
    char *next[] = {"second-argv-zero", "after-self-exec", NULL};
    execve("/proc/self/exe", next, environ);
    return errno;
  }
  return argc == 2 && strcmp(argv[1], "after-self-exec") == 0 ? 0 : 21;
}
"#,
    );
    let alias = root.0.join("alias-name");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let launcher_source = format!(
        r#"
#include <errno.h>
#include <unistd.h>

int main(void) {{
  char *argv[] = {{"not-the-path", NULL}};
  char *envp[] = {{NULL}};
  execve("{}", argv, envp);
  return errno;
}}
"#,
        alias.display(),
    );
    let launcher = compile_c_program(&root.0, "comm-launcher", &launcher_source);
    let launcher = launcher.to_str().unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(launcher).unwrap(),
            &[launcher],
            &["PATH=/usr/bin:/bin"],
            &root.0,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(
        code,
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let target = target.canonicalize().unwrap();
    let expected = format!(
        "{target}\nalias-name\nnot-the-path\n{target}\nexe\nsecond-argv-zero\n",
        target = target.display(),
    );
    assert_eq!(stdout, expected.as_bytes());
    assert!(stderr.is_empty());
}

fn leader_self_exec_bounded(test: &str) -> bool {
    if !kvm_available(test) {
        return false;
    }
    if std::env::var("REVERIE_LEADER_EXEC_CHILD").as_deref() == Ok(test) {
        return true;
    }
    let output = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "30s"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("REVERIE_LEADER_EXEC_CHILD", test)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{test}: status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn leader_self_exec_guest(
    directory: &std::path::Path,
    program: &std::path::Path,
    arguments: &[&str],
    expected: &[u8],
) {
    let mut argv = vec![program.to_str().unwrap()];
    argv.extend_from_slice(arguments);
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(program).unwrap(),
            &argv,
            &[],
            directory,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(
        code,
        0,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(stdout, expected);
    assert!(
        stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&stderr)
    );
}

fn leader_self_exec_identity_control(test: &str, case: &str) {
    if !leader_self_exec_bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let program = compile_c_program(
        &directory.0,
        "running-image",
        LEADER_SELF_EXEC_IDENTITY_PROGRAM,
    );
    compile_c_program(
        &directory.0,
        "running-image.replacement",
        "int main(void) { return 77; }",
    );
    let expected = if case == "noexec" {
        "exec_returned errno=13 comm_full16_preserved=1\n".to_owned()
    } else {
        format!(
            "identity=1 link_full4096=1 argv=1 comm_full16=1 comm=657865{}\n",
            "00".repeat(13)
        )
    };
    leader_self_exec_guest(&directory.0, &program, &[case], expected.as_bytes());
}

fn leader_self_exec_name_control(test: &str, case: &str) {
    if !leader_self_exec_bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let program = compile_c_program(
        &directory.0,
        "actual-image-name-longer-than-15",
        LEADER_SELF_EXEC_NAME_PROGRAM,
    );
    let invoked = match case {
        "direct" => program.clone(),
        "alias" => {
            let alias = directory.0.join("chosen-alias");
            std::os::unix::fs::symlink(&program, &alias).unwrap();
            alias
        }
        "script" => {
            use std::os::unix::fs::PermissionsExt;
            let script = directory.0.join("chosen-script");
            std::fs::write(&script, format!("#!{}\n", program.display())).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            script
        }
        _ => unreachable!(),
    };
    let name = invoked.file_name().unwrap().to_str().unwrap();
    let mut expected_name = [0u8; 16];
    let count = name.len().min(15);
    expected_name[..count].copy_from_slice(&name.as_bytes()[..count]);
    let hex = expected_name
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let expected = format!("comm_full16=1 comm={hex}\n");
    leader_self_exec_guest(
        &directory.0,
        &program,
        &[invoked.to_str().unwrap(), name],
        expected.as_bytes(),
    );
}

fn leader_self_exec_mutable_control(test: &str, method: &str, case: &str) {
    if !leader_self_exec_bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let program = compile_c_program(
        &directory.0,
        "running-image",
        LEADER_SELF_EXEC_MUTABLE_PROGRAM,
    );
    compile_c_program(&directory.0, "replacement", "int main(void) { return 77; }");
    let alias = directory.0.join("alias\n\\stage");
    std::os::unix::fs::symlink(&program, &alias).unwrap();
    let (mutation, request, name) = if case == "alias" {
        ("normal", alias.to_str().unwrap(), "alias\n\\stage")
    } else {
        (case, "/proc/self/exe", "exe")
    };
    let expected = format!(
        "method={method} mutation={mutation} name-reset argv-env image identity mutable-name thread checks=PASS\n"
    );
    leader_self_exec_guest(
        &directory.0,
        &program,
        &[method, mutation, request, name, program.to_str().unwrap()],
        expected.as_bytes(),
    );
}

#[test]
fn leader_self_exec_identity_plain() {
    leader_self_exec_identity_control("leader_self_exec_identity_plain", "plain");
}

#[test]
fn leader_self_exec_identity_unlink() {
    leader_self_exec_identity_control("leader_self_exec_identity_unlink", "unlink");
}

#[test]
fn leader_self_exec_identity_replace() {
    leader_self_exec_identity_control("leader_self_exec_identity_replace", "replace");
}

#[test]
fn leader_self_exec_identity_noexec() {
    leader_self_exec_identity_control("leader_self_exec_identity_noexec", "noexec");
}

#[test]
fn leader_self_exec_identity_execute_only() {
    leader_self_exec_identity_control("leader_self_exec_identity_execute_only", "execute-only");
}

#[test]
fn leader_self_exec_name_direct() {
    leader_self_exec_name_control("leader_self_exec_name_direct", "direct");
}

#[test]
fn leader_self_exec_name_alias() {
    leader_self_exec_name_control("leader_self_exec_name_alias", "alias");
}

#[test]
fn leader_self_exec_name_script() {
    leader_self_exec_name_control("leader_self_exec_name_script", "script");
}

#[test]
fn leader_self_exec_mutable_0_normal() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_0_normal", "0", "normal");
}

#[test]
fn leader_self_exec_mutable_0_alias() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_0_alias", "0", "alias");
}

#[test]
fn leader_self_exec_mutable_0_unlink() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_0_unlink", "0", "unlink");
}

#[test]
fn leader_self_exec_mutable_0_replace() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_0_replace", "0", "replace");
}

#[test]
fn leader_self_exec_mutable_1_normal() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_1_normal", "1", "normal");
}

#[test]
fn leader_self_exec_mutable_1_alias() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_1_alias", "1", "alias");
}

#[test]
fn leader_self_exec_mutable_1_unlink() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_1_unlink", "1", "unlink");
}

#[test]
fn leader_self_exec_mutable_1_replace() {
    leader_self_exec_mutable_control("leader_self_exec_mutable_1_replace", "1", "replace");
}

const LEADER_SELF_EXEC_IDENTITY_PROGRAM: &str = r###"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *stage = getenv("W12_STAGE");
    if (stage) {
        struct stat actual;
        if (stat("/proc/self/exe", &actual)) return 50;
        int identity = (unsigned long long)actual.st_dev == strtoull(getenv("W12_DEV"), NULL, 10)
            && (unsigned long long)actual.st_ino == strtoull(getenv("W12_INO"), NULL, 10);
        unsigned char comm[16], expected_comm[16] = {'e','x','e',0};
        memset(comm, 0x5a, sizeof(comm));
        if (prctl(PR_GET_NAME, comm)) return 51;
        unsigned char actual_link[PATH_MAX], expected_link[PATH_MAX];
        memset(actual_link, 0x5a, sizeof(actual_link));
        memset(expected_link, 0x5a, sizeof(expected_link));
        const char *expected = getenv("W12_LINK");
        size_t length = strlen(expected);
        if (length >= sizeof(expected_link)) return 52;
        memcpy(expected_link, expected, length);
        ssize_t count = readlink("/proc/self/exe", (char *)actual_link, sizeof(actual_link));
        int link_equal = count == (ssize_t)length && !memcmp(actual_link, expected_link, sizeof(actual_link));
        int argv_equal = argc == 2 && !strcmp(argv[0], "not-the-image") && !strcmp(argv[1], "after");
        int comm_equal = !memcmp(comm, expected_comm, sizeof(comm));
        printf("identity=%d link_full4096=%d argv=%d comm_full16=%d comm=", identity, link_equal, argv_equal, comm_equal);
        for (size_t index=0; index<sizeof(comm); ++index) printf("%02x", comm[index]);
        putchar('\n');
        return identity && link_equal && argv_equal && comm_equal ? 0 : 53;
    }
    if (argc != 2) return 10;
    struct stat before;
    if (stat("/proc/self/exe", &before)) return 11;
    int deleted = !strcmp(argv[1], "unlink") || !strcmp(argv[1], "replace");
    char link[PATH_MAX+32], dev[96], inode[96];
    if (snprintf(link,sizeof(link),"W12_LINK=%s%s",argv[0],deleted ? " (deleted)" : "") >= (int)sizeof(link)) return 12;
    snprintf(dev,sizeof(dev),"W12_DEV=%llu",(unsigned long long)before.st_dev);
    snprintf(inode,sizeof(inode),"W12_INO=%llu",(unsigned long long)before.st_ino);
    if (!strcmp(argv[1], "unlink") && unlink(argv[0])) return 13;
    if (!strcmp(argv[1], "replace")) {
        char replacement[PATH_MAX];
        if (snprintf(replacement,sizeof(replacement),"%s.replacement",argv[0]) >= (int)sizeof(replacement)) return 14;
        if (rename(replacement,argv[0])) return 15;
    }
    if (!strcmp(argv[1], "noexec") && chmod(argv[0],0600)) return 16;
    if (!strcmp(argv[1], "execute-only") && chmod(argv[0],0100)) return 22;
    if (prctl(PR_SET_NAME,"before-reexec")) return 17;
    char *arguments[] = {"not-the-image", "after", NULL};
    char *environment[] = {"W12_STAGE=1", dev, inode, link, NULL};
    errno=0;
    execve("/proc/self/exe", arguments, environment);
    int error=errno;
    unsigned char comm[16], expected_comm[16] = "before-reexec";
    memset(comm,0x5a,sizeof(comm));
    if (prctl(PR_GET_NAME,comm)) return 18;
    int preserved=!memcmp(comm,expected_comm,sizeof(comm));
    printf("exec_returned errno=%d comm_full16_preserved=%d\n",error,preserved);
    if (!strcmp(argv[1], "noexec")) return error == EACCES && preserved ? 0 : 19;
    return 20;
}
"###;

const LEADER_SELF_EXEC_NAME_PROGRAM: &str = r###"
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *expected = getenv("W12_EXPECT_NAME");
    if (expected) {
        unsigned char actual[16], wanted[16] = {0};
        size_t length = strlen(expected);
        if (length > 15) length = 15;
        memcpy(wanted, expected, length);
        memset(actual,0x5a,sizeof(actual));
        if (prctl(PR_GET_NAME,actual)) return 20;
        int equal=!memcmp(actual,wanted,sizeof(actual));
        printf("comm_full16=%d comm=",equal);
        for (size_t index=0; index<sizeof(actual); ++index) printf("%02x",actual[index]);
        putchar('\n');
        return equal ? 0 : 21;
    }
    if (argc != 3) return 10;
    if (prctl(PR_SET_NAME,"mutated-before")) return 11;
    char setting[256];
    if (snprintf(setting,sizeof(setting),"W12_EXPECT_NAME=%s",argv[2]) >= (int)sizeof(setting)) return 12;
    char *arguments[] = {"misleading-argv0", "payload", NULL};
    char *environment[] = {setting,NULL};
    execve(argv[1],arguments,environment);
    perror("execve");
    return 13;
}
"###;

const LEADER_SELF_EXEC_MUTABLE_PROGRAM: &str = r###"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

static volatile unsigned image_value = 0x12345678;
static const char renamed[] = "after\n\\name";
static int failures;
static void require(int condition, const char *what) {
    if (!condition) { printf("FAIL %s errno=%d\n", what, errno); ++failures; }
}
static int name_is(const char *name) {
    unsigned char actual[64], expected[64];
    memset(actual, 0x5a, sizeof(actual)); memset(expected, 0x5a, sizeof(expected));
    memset(expected, 0, 16); memcpy(expected, name, strlen(name) < 15 ? strlen(name) : 15);
    return prctl(PR_GET_NAME, actual, 0, 0, 0) == 0 && !memcmp(actual, expected, sizeof(actual));
}
static int status_name_is(const char *name) {
    unsigned char actual[16384]; memset(actual, 0x5a, sizeof(actual));
    char expected[128] = "Name:\t"; size_t expected_length = 6;
    for (size_t index = 0; name[index] && index < 15; ++index) {
        if (name[index] == '\n') { expected[expected_length++] = '\\'; expected[expected_length++] = 'n'; }
        else if (name[index] == '\\') { expected[expected_length++] = '\\'; expected[expected_length++] = '\\'; }
        else expected[expected_length++] = name[index];
    }
    expected[expected_length++] = '\n';
    int descriptor = open("/proc/self/status", O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) return 0;
    ssize_t count = read(descriptor, actual, sizeof(actual)); close(descriptor);
    if (count < (ssize_t)expected_length || memcmp(actual, expected, expected_length)) return 0;
    for (size_t index = (size_t)count; index < sizeof(actual); ++index) if (actual[index] != 0x5a) return 0;
    return 1;
}
static void *thread_name(void *unused) {
    (void)unused;
    if (!name_is(renamed) || prctl(PR_SET_NAME, "worker-private", 0, 0, 0) || !name_is("worker-private") || !status_name_is(renamed)) return (void *)1;
    return NULL;
}
static long execute(int method, const char *path, char **arguments, char **environment) {
    if (method) return syscall(SYS_execveat, AT_FDCWD, path, arguments, environment, 0);
    return syscall(SYS_execve, path, arguments, environment);
}
int main(int argc, char **argv) {
    if (argc == 7 && !strcmp(argv[1], "after")) {
        require(!strcmp(argv[0], "different-argv-zero") && !strcmp(argv[6], "argument-retained"), "argv bytes");
        require(getenv("LEADER_TEST_ENV") && !strcmp(getenv("LEADER_TEST_ENV"), "preserved"), "environment");
        require(image_value == 0x12345678, "new image data reset");
        require(name_is(argv[3]), "native exec filename name reset");
        require(status_name_is(argv[3]), "initial status name");
        unsigned char actual[4096], expected[4096];
        memset(actual, 0x5a, sizeof(actual)); memset(expected, 0x5a, sizeof(expected));
        size_t expected_length = strlen(argv[4]); memcpy(expected, argv[4], expected_length);
        if (strcmp(argv[5], "normal")) { memcpy(expected+expected_length, " (deleted)", 10); expected_length += 10; }
        ssize_t length = readlink("/proc/self/exe", (char *)actual, sizeof(actual));
        require(length == (ssize_t)expected_length && !memcmp(actual, expected, sizeof(actual)), "retained executable identity full4096");
        require(prctl(PR_SET_NAME, renamed, 0, 0, 0) == 0 && name_is(renamed), "mutable name after exec full64");
        require(status_name_is(renamed), "escaped status after exec");
        pthread_t worker; void *result = (void *)1;
        int created = pthread_create(&worker, NULL, thread_name, NULL);
        require(created == 0, "post-exec thread creation");
        if (!created) require(pthread_join(worker, &result) == 0 && result == NULL, "per-thread name and leader status");
        require(name_is(renamed) && status_name_is(renamed), "leader name preserved after thread");
        printf("method=%s mutation=%s name-reset argv-env image identity mutable-name thread checks=%s\n", argv[2], argv[5], failures ? "FAIL" : "PASS");
        return failures ? 1 : 0;
    }
    if (argc != 6) return 90;
    int method = atoi(argv[1]);
    require(prctl(PR_SET_NAME, "before-exec", 0, 0, 0) == 0 && name_is("before-exec"), "pre-exec name");
    image_value = 0x87654321;
    char *environment[] = {"LEADER_TEST_ENV=preserved", "PATH=/usr/bin:/bin", NULL};
    char *arguments[] = {"different-argv-zero", "after", argv[1], argv[4], argv[5], argv[2], "argument-retained", NULL};
    if (!strcmp(argv[2], "unlink")) require(unlink(argv[5]) == 0, "unlink current image");
    if (!strcmp(argv[2], "replace")) require(rename("replacement", argv[5]) == 0, "replace current image");
    if (failures) return 91;
    execute(method, argv[3], arguments, environment);
    printf("FAIL exec returned method=%d errno=%d name-unchanged=%d\n", method, errno, name_is("before-exec"));
    return 92;
}
"###;

#[test]
fn proc_root_retains_fchmodat2_native_control() {
    assert!(kvm_available("proc_root_retains_fchmodat2_native_control"));
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "review-535-fchmodat2",
        PROC_ROOT_FCHMODAT2_PROGRAM,
    );
    let native = std::process::Command::new(&executable)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    println!(
        "NATIVE status={:?} stdout={} stderr={}",
        native.status.code(),
        String::from_utf8_lossy(&native.stdout),
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(native.status.code(), Some(0));
    assert!(native.stdout.is_empty() && native.stderr.is_empty());
    let image = std::fs::read(&executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable.to_str().unwrap()],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(
        code,
        0,
        "KVM stdout={} stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert!(stdout.is_empty() && stderr.is_empty());
}

const PROC_ROOT_FCHMODAT2_PROGRAM: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#ifndef SYS_fchmodat2
#define SYS_fchmodat2 452
#endif

static int change(int descriptor, const char *path, unsigned mode, int flags, int expected_errno) {
    errno = 0;
    long result = syscall(SYS_fchmodat2, descriptor, path, mode, flags);
    if (result != (expected_errno ? -1 : 0) || errno != expected_errno) {
        printf("path=%s flags=%d result=%ld errno=%d expected_errno=%d\n", path, flags, result, errno, expected_errno);
        return 1;
    }
    return 0;
}

int main(void) {
    unsigned char expected[128], actual[128];
    for (unsigned index = 0; index < sizeof(expected); ++index) expected[index] = (unsigned char)(index ^ 0xa5);
    int file = open("payload", O_CREAT | O_EXCL | O_RDWR, 0600);
    if (file < 0 || write(file, expected, sizeof(expected)) != sizeof(expected)) return 80;
    if (mkdir("directory", 0700) || symlink("directory", "alias")) return 81;
    if (change(AT_FDCWD, "directory///", 0711, 0, 0)) return 82;
    if (change(AT_FDCWD, "alias///", 0712, AT_SYMLINK_NOFOLLOW, 0)) return 83;
    if (change(AT_FDCWD, "payload///", 0777, 0, ENOTDIR)) return 84;
    if (change(file, "", 0601, 0, ENOENT)) return 85;
    if (change(file, "", 0601, AT_EMPTY_PATH, 0)) return 86;
    if (change(-1, "directory///", 0777, 0, EBADF)) return 87;
    if (change(-1, "directory///", 0777, 0x40000000, EINVAL)) return 88;
    char absolute[8192], cwd[4096];
    if (!getcwd(cwd, sizeof(cwd)) || snprintf(absolute, sizeof(absolute), "%s/directory///", cwd) >= sizeof(absolute)) return 89;
    if (change(-1, absolute, 0713, 0, 0)) return 90;
    struct stat metadata;
    if (fstat(file, &metadata) || (metadata.st_mode & 07777) != 0601) return 91;
    if (lseek(file, 0, SEEK_SET) != 0 || read(file, actual, sizeof(actual)) != sizeof(actual) || memcmp(actual, expected, sizeof(actual))) return 92;
    if (stat("directory", &metadata) || (metadata.st_mode & 07777) != 0713) return 93;
    char link[32] = {0};
    if (readlink("alias", link, sizeof(link)) != 9 || memcmp(link, "directory", 9)) return 94;
    close(file);
    unlink("alias");
    unlink("payload");
    rmdir("directory");
    return 0;
}
"#;

#[test]
fn proc_root_original_mutation_vectors_match_native() {
    assert!(kvm_available(
        "proc_root_original_mutation_vectors_match_native"
    ));
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "proc-root-mutations",
        PROC_ROOT_MUTATION_PROGRAM,
    );
    let native = std::process::Command::new(&executable).output().unwrap();
    println!(
        "native original vectors: {}",
        String::from_utf8_lossy(&native.stdout)
    );
    assert_eq!(
        native.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert!(native.stderr.is_empty());
    let image = std::fs::read(&executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable.to_str().unwrap()],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    println!(
        "guest original vectors: {}",
        String::from_utf8_lossy(&stdout)
    );
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(code, 0);
}

const PROC_ROOT_MUTATION_PROGRAM: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
int main(void) {
    char directory[] = "/tmp/reverie-proc-mutation-XXXXXX";
    if (!mkdtemp(directory) || chdir(directory)) return 80;
    int file = open("local-source", O_CREAT | O_EXCL | O_WRONLY, 0600);
    if (file < 0 || write(file, "payload", 7) != 7 || close(file) || mkdir("removable", 0700)) return 81;
    int proc = open("/proc", O_RDONLY | O_DIRECTORY);
    if (proc < 0) return 82;
    char missing[4096], single_slash[4096], source[4096], removable[4096];
    if (snprintf(missing, sizeof(missing), "%s/escaped///", directory + 1) >= sizeof(missing)) return 83;
    if (snprintf(single_slash, sizeof(single_slash), "%s/escaped/", directory + 1) >= sizeof(single_slash)) return 83;
    if (snprintf(source, sizeof(source), "%s/local-source", directory + 1) >= sizeof(source)) return 83;
    if (snprintf(removable, sizeof(removable), "%s/removable///", directory + 1) >= sizeof(removable)) return 83;
    struct operation { const char *name; long number; long args[6]; } operations[] = {
        {"mkdirat single slash", SYS_mkdirat, {proc, (long)single_slash, 0755}},
        {"mkdirat", SYS_mkdirat, {proc, (long)missing, 0755}},
        {"unlinkat", SYS_unlinkat, {proc, (long)removable, AT_REMOVEDIR}},
        {"renameat source", SYS_renameat, {proc, (long)source, AT_FDCWD, (long)"local-destination"}},
        {"renameat destination", SYS_renameat, {AT_FDCWD, (long)"local-source", proc, (long)missing}},
        {"renameat2 source", SYS_renameat2, {proc, (long)source, AT_FDCWD, (long)"local-destination"}},
        {"renameat2 destination", SYS_renameat2, {AT_FDCWD, (long)"local-source", proc, (long)missing}},
        {"linkat source", SYS_linkat, {proc, (long)source, AT_FDCWD, (long)"local-destination"}},
        {"linkat destination", SYS_linkat, {AT_FDCWD, (long)"local-source", proc, (long)missing}},
        {"symlinkat", SYS_symlinkat, {(long)"local-source", proc, (long)missing}},
        {"fchmodat", SYS_fchmodat, {proc, (long)source, 0777}},
        {"mknodat", SYS_mknodat, {proc, (long)missing, S_IFIFO | 0600}},
        {"utimensat", SYS_utimensat, {proc, (long)source}},
    };
    int failures = 0;
    for (unsigned index = 0; index < sizeof(operations) / sizeof(operations[0]); ++index) {
        const struct operation *operation = &operations[index];
        errno = 0;
        long result = syscall(operation->number, operation->args[0], operation->args[1], operation->args[2], operation->args[3], operation->args[4], operation->args[5]);
        printf("%s result=%ld errno=%d\n", operation->name, result, errno);
        if (result != -1 || errno != ENOENT) ++failures;
        struct stat metadata;
        if (stat("local-source", &metadata) || (metadata.st_mode & 0777) != 0600 || metadata.st_size != 7) return 84;
        file = open("local-source", O_RDONLY);
        char bytes[16], expected[16];
        memset(bytes, 0xa5, sizeof(bytes));
        memset(expected, 0xa5, sizeof(expected));
        memcpy(expected, "payload", 7);
        if (file < 0 || read(file, bytes, sizeof(bytes)) != 7 || memcmp(bytes, expected, sizeof(bytes)) || close(file)) return 85;
        if (stat("removable", &metadata) || !S_ISDIR(metadata.st_mode)) return 86;
        errno = 0;
        if (lstat("escaped", &metadata) != -1 || errno != ENOENT) return 87;
        errno = 0;
        if (lstat("local-destination", &metadata) != -1 || errno != ENOENT) return 88;
    }
    if (close(proc) || unlink("local-source") || rmdir("removable") || chdir("/") || rmdir(directory)) return 89;
    return failures ? 94 : 0;
}
"#;

fn proc_root_consumer_case(case: usize) {
    assert!(kvm_available("proc_root_consumer_case"));
    let directory = TestDirectory::new();
    let program = format!("#define CASE {case}\n{}", PROC_ROOT_CONSUMER_PROGRAM);
    let executable = compile_c_program(&directory.0, "proc-root-consumer", &program);
    let native = std::process::Command::new(&executable)
        .arg("native")
        .current_dir(&directory.0)
        .output()
        .unwrap();
    println!(
        "native consumer={case}: {:?} {}",
        native.status.code(),
        String::from_utf8_lossy(&native.stdout)
    );
    assert_eq!(
        native.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert!(native.stderr.is_empty());
    let image = std::fs::read(&executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable.to_str().unwrap(), "guest"],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    println!(
        "guest consumer={case}: {code} {}",
        String::from_utf8_lossy(&stdout)
    );
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(code, 0);
}

#[test]
fn proc_root_received_full_metadata() {
    proc_root_consumer_case(0);
}
#[test]
fn proc_root_received_empty_enumeration() {
    proc_root_consumer_case(1);
}
#[test]
fn proc_root_received_allowlisted_contents() {
    proc_root_consumer_case(2);
}
#[test]
fn proc_root_received_aliases_and_reuse() {
    proc_root_consumer_case(3);
}
#[test]
fn proc_root_received_fork_exec() {
    proc_root_consumer_case(4);
}
#[test]
fn proc_root_received_filesystem_stat_refusal() {
    proc_root_consumer_case(5);
}
#[test]
fn proc_root_cwd_restriction_and_ordinary_positive() {
    proc_root_consumer_case(6);
}
#[test]
fn proc_root_received_shared_thread_files() {
    proc_root_consumer_case(7);
}

const PROC_ROOT_CONSUMER_PROGRAM: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(expression) do { if (!(expression)) { printf("failure line=%d errno=%d\n", __LINE__, errno); return 93; } } while (0)

static int transfer(int descriptor, int cloexec) {
    int sockets[2];
    CHECK(socketpair(AF_UNIX, SOCK_DGRAM, 0, sockets) == 0);
    char payload = 'x';
    char control[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec vector = {.iov_base = &payload, .iov_len = 1};
    struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof(control)};
    struct cmsghdr *header = CMSG_FIRSTHDR(&message);
    header->cmsg_level = SOL_SOCKET;
    header->cmsg_type = SCM_RIGHTS;
    header->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(header), &descriptor, sizeof(int));
    CHECK(sendmsg(sockets[0], &message, 0) == 1);
    CHECK(close(descriptor) == 0);
    memset(control, 0, sizeof(control));
    message.msg_controllen = sizeof(control);
    payload = 0;
    CHECK(recvmsg(sockets[1], &message, cloexec ? MSG_CMSG_CLOEXEC : 0) == 1);
    CHECK(payload == 'x' && !(message.msg_flags & MSG_CTRUNC));
    header = CMSG_FIRSTHDR(&message);
    CHECK(header && header->cmsg_level == SOL_SOCKET && header->cmsg_type == SCM_RIGHTS && header->cmsg_len == CMSG_LEN(sizeof(int)));
    memcpy(&descriptor, CMSG_DATA(header), sizeof(int));
    CHECK(close(sockets[0]) == 0 && close(sockets[1]) == 0);
    CHECK(fcntl(descriptor, F_GETFD) == (cloexec ? FD_CLOEXEC : 0));
    return descriptor;
}

static int metadata(int descriptor, int baseline, int guest) {
    struct stat actual, expected;
    memset(&actual, 0xa5, sizeof(actual));
    memset(&expected, 0xa5, sizeof(expected));
    CHECK(fstat(descriptor, &actual) == 0 && fstat(baseline, &expected) == 0);
    CHECK(S_ISDIR(actual.st_mode));
    if (guest) CHECK(memcmp(&actual, &expected, sizeof(actual)) == 0);
    struct statx actualx, expectedx;
    memset(&actualx, 0xa5, sizeof(actualx));
    memset(&expectedx, 0xa5, sizeof(expectedx));
    CHECK(statx(descriptor, "", AT_EMPTY_PATH, STATX_BASIC_STATS, &actualx) == 0);
    CHECK(statx(baseline, "", AT_EMPTY_PATH, STATX_BASIC_STATS, &expectedx) == 0);
    if (guest) CHECK(memcmp(&actualx, &expectedx, sizeof(actualx)) == 0);
    struct stat empty;
    memset(&empty, 0xa5, sizeof(empty));
    CHECK(fstatat(descriptor, "", &empty, AT_EMPTY_PATH) == 0);
    if (guest) CHECK(memcmp(&empty, &expected, sizeof(empty)) == 0);
    char path[128], target[1024], wanted[1024];
    CHECK(snprintf(path, sizeof(path), "/proc/self/fd/%d", descriptor) < sizeof(path));
    memset(target, 0xa5, sizeof(target));
    memset(wanted, 0xa5, sizeof(wanted));
    memcpy(wanted, "/proc", 5);
    CHECK(readlink(path, target, sizeof(target)) == 5);
    CHECK(memcmp(target, wanted, sizeof(target)) == 0);
    return 0;
}

static int enumeration(int descriptor, int guest) {
    unsigned char output[16384], before[16384];
    memset(output, 0xa5, sizeof(output));
    memcpy(before, output, sizeof(output));
    long result = syscall(SYS_getdents64, descriptor, output, sizeof(output));
    if (guest) {
        CHECK(result == 0);
        CHECK(memcmp(output, before, sizeof(output)) == 0);
    } else {
        CHECK(result > 0);
    }
    return 0;
}

static int contents(int descriptor, int baseline, int guest) {
    const char *paths[] = {"version", "sys/kernel/osrelease", "self/cmdline"};
    for (unsigned index = 0; index < sizeof(paths) / sizeof(paths[0]); ++index) {
        int actual = openat(descriptor, paths[index], O_RDONLY);
        int expected = openat(baseline, paths[index], O_RDONLY);
        CHECK(actual >= 0 && expected >= 0);
        unsigned char actual_bytes[16384], expected_bytes[16384];
        memset(actual_bytes, 0xa5, sizeof(actual_bytes));
        memset(expected_bytes, 0xa5, sizeof(expected_bytes));
        ssize_t actual_size = read(actual, actual_bytes, sizeof(actual_bytes));
        ssize_t expected_size = read(expected, expected_bytes, sizeof(expected_bytes));
        CHECK(actual_size > 0 && actual_size < sizeof(actual_bytes));
        CHECK(actual_size == expected_size && memcmp(actual_bytes, expected_bytes, sizeof(actual_bytes)) == 0);
        CHECK(close(actual) == 0 && close(expected) == 0);
    }
    if (guest) {
        const char *unlisted[] = {"../etc/passwd", "self/environ", "thread-self/environ", "self/task", "self/fd", "1/environ"};
        for (unsigned index = 0; index < sizeof(unlisted) / sizeof(unlisted[0]); ++index) {
            errno = 0;
            CHECK(openat(descriptor, unlisted[index], O_RDONLY) == -1 && errno == ENOENT);
        }
    }
    return 0;
}

struct shared_context { int baseline; int guest; };

static void *shared_worker(void *opaque) {
    struct shared_context *context = opaque;
    if (close(64)) return (void *)(uintptr_t)1;
    int descriptor = open("/proc", O_RDONLY | O_DIRECTORY);
    if (descriptor < 0 || dup2(descriptor, 65) != 65 || close(descriptor)) return (void *)(uintptr_t)2;
    return (void *)(uintptr_t)metadata(65, context->baseline, context->guest);
}

static int filesystem_stats(int descriptor, int guest) {
    char alias[128];
    CHECK(snprintf(alias, sizeof(alias), "/proc/self/fd/%d", descriptor) < sizeof(alias));
    for (int variant = 0; variant < 3; ++variant) {
        unsigned char output[sizeof(struct statfs)], before[sizeof(struct statfs)];
        memset(output, 0xa5, sizeof(output));
        memcpy(before, output, sizeof(output));
        errno = 0;
        int result = variant == 0 ? fstatfs(descriptor, (struct statfs *)output) : statfs(variant == 1 ? alias : "/proc", (struct statfs *)output);
        printf("statfs variant=%d result=%d errno=%d\n", variant, result, errno);
        if (guest) {
            CHECK(result == -1 && errno == EACCES);
            CHECK(memcmp(output, before, sizeof(output)) == 0);
        } else {
            CHECK(result == 0);
        }
    }
    struct statfs ordinary;
    CHECK(statfs(".", &ordinary) == 0);
    int ordinary_fd = open(".", O_RDONLY | O_DIRECTORY);
    CHECK(ordinary_fd >= 0 && fstatfs(ordinary_fd, &ordinary) == 0);
    errno = 0;
    CHECK(syscall(SYS_fstatfs, ordinary_fd, (void *)1) == -1 && errno == EFAULT);
    CHECK(close(ordinary_fd) == 0);
    errno = 0;
    CHECK(syscall(SYS_fstatfs, -1, (void *)1) == -1 && errno == EBADF);
    return 0;
}

int main(int argc, char **argv) {
    CHECK(argc >= 2);
    int guest = strcmp(argv[1], "guest") == 0;
    if (argc == 3) {
        int baseline = open("/proc", O_RDONLY | O_DIRECTORY);
        CHECK(baseline >= 0 && metadata(60, baseline, guest) == 0);
        CHECK(contents(60, baseline, guest) == 0);
        errno = 0;
        CHECK(fcntl(61, F_GETFD) == -1 && errno == EBADF);
        return 0;
    }
    int descriptor = open("/proc", O_RDONLY | O_DIRECTORY);
    CHECK(descriptor >= 0);
    descriptor = transfer(descriptor, 1);
    CHECK(descriptor >= 0 && descriptor != 93);
    descriptor = transfer(descriptor, 0);
    CHECK(descriptor >= 0 && descriptor != 93);
    int baseline = open("/proc", O_RDONLY | O_DIRECTORY);
    CHECK(baseline >= 0);
    if (CASE == 0) CHECK(metadata(descriptor, baseline, guest) == 0);
    if (CASE == 1) CHECK(enumeration(descriptor, guest) == 0);
    if (CASE == 2) CHECK(contents(descriptor, baseline, guest) == 0);
    if (CASE == 3) {
        int copies[3] = {dup(descriptor), fcntl(descriptor, F_DUPFD_CLOEXEC, 20), dup2(descriptor, 24)};
        CHECK(close(descriptor) == 0);
        for (unsigned index = 0; index < 3; ++index) {
            CHECK(copies[index] >= 0 && metadata(copies[index], baseline, guest) == 0);
            CHECK(contents(copies[index], baseline, guest) == 0);
        }
        const char *prefixes[] = {"/dev/fd/", "/proc/self/fd/", "/proc/thread-self/fd/"};
        for (unsigned index = 0; index < 3; ++index) {
            char path[128];
            CHECK(snprintf(path, sizeof(path), "%s%d", prefixes[index], copies[0]) < sizeof(path));
            int reopened = open(path, O_RDONLY | O_DIRECTORY);
            CHECK(reopened >= 0 && metadata(reopened, baseline, guest) == 0);
            CHECK(contents(reopened, baseline, guest) == 0);
            CHECK(close(reopened) == 0);
        }
        char numeric_path[128];
        CHECK(snprintf(numeric_path, sizeof(numeric_path), "/proc/%d/fd/%d", getpid(), copies[0]) < sizeof(numeric_path));
        int numeric = open(numeric_path, O_RDONLY | O_DIRECTORY);
        CHECK(numeric >= 0 && metadata(numeric, baseline, guest) == 0);
        CHECK(contents(numeric, baseline, guest) == 0 && close(numeric) == 0);
        int ordinary = open("/", O_RDONLY | O_DIRECTORY);
        CHECK(ordinary >= 0 && dup2(ordinary, copies[0]) == copies[0]);
        char path[128], target[1024];
        CHECK(snprintf(path, sizeof(path), "/proc/self/fd/%d", copies[0]) < sizeof(path));
        CHECK(readlink(path, target, sizeof(target)) == 1 && target[0] == '/');
    }
    if (CASE == 4) {
        CHECK(dup2(descriptor, 60) == 60 && dup3(descriptor, 61, O_CLOEXEC) == 61);
        pid_t child = fork();
        CHECK(child >= 0);
        if (child == 0) {
            execl(argv[0], argv[0], argv[1], "after-exec", (char *)0);
            _exit(94);
        }
        int status;
        CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
        CHECK(metadata(descriptor, baseline, guest) == 0);
    }
    if (CASE == 5) CHECK(filesystem_stats(descriptor, guest) == 0);
    if (CASE == 6) {
        char before[4096], after[4096];
        CHECK(getcwd(before, sizeof(before)) != NULL);
        int saved = open(".", O_RDONLY | O_DIRECTORY);
        CHECK(saved >= 0);
        errno = 0;
        int result = fchdir(descriptor);
        printf("fchdir result=%d errno=%d\n", result, errno);
        if (guest) {
            CHECK(result == -1 && errno == EACCES);
            CHECK(getcwd(after, sizeof(after)) && strcmp(before, after) == 0);
        } else {
            CHECK(result == 0 && getcwd(after, sizeof(after)) && strcmp(after, "/proc") == 0);
        }
        CHECK(fchdir(saved) == 0);
        CHECK(getcwd(after, sizeof(after)) && strcmp(before, after) == 0);
        int path_fd = open(".", O_PATH | O_DIRECTORY);
        CHECK(path_fd >= 0);
        errno = 0;
        result = fchdir(path_fd);
        printf("O_PATH fchdir result=%d errno=%d\n", result, errno);
        if (guest) CHECK(result == -1 && errno == EBADF);
        else CHECK(result == 0);
        CHECK(close(path_fd) == 0 && close(saved) == 0);
    }
    if (CASE == 7) {
        CHECK(dup2(descriptor, 64) == 64);
        struct shared_context context = {.baseline = baseline, .guest = guest};
        pthread_t thread;
        CHECK(pthread_create(&thread, NULL, shared_worker, &context) == 0);
        void *result;
        CHECK(pthread_join(thread, &result) == 0 && result == NULL);
        errno = 0;
        CHECK(fcntl(64, F_GETFD) == -1 && errno == EBADF);
        CHECK(metadata(65, baseline, guest) == 0 && contents(65, baseline, guest) == 0);
    }
    return 0;
}
"#;
const LOAD_ADDRESS: u64 = 0x20_0000;
const CODE_OFFSET: usize = 0x1000;
const POST_EXEC_RANDOM: [u8; 16] = *b"kvm-post-exec-ok";
static POST_EXEC_FAILURE_EXITED: AtomicBool = AtomicBool::new(false);

fn proc_root_path_case(case: usize) {
    assert!(kvm_available("proc_root_path_case"));
    let directory = TestDirectory::new();
    let program = format!("#define CASE {case}\n{}", PROC_ROOT_PATH_PROGRAM);
    let executable = compile_c_program(&directory.0, "proc-root-path", &program);
    let native = std::process::Command::new(&executable)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    println!(
        "native case={case}: {:?} {}",
        native.status.code(),
        String::from_utf8_lossy(&native.stdout)
    );
    assert_eq!(
        native.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert!(native.stderr.is_empty());
    let image = std::fs::read(&executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable.to_str().unwrap()],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    println!(
        "guest case={case}: {code} {}",
        String::from_utf8_lossy(&stdout)
    );
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(code, 0);
}

#[test]
fn proc_root_local_trailing_positive() {
    proc_root_path_case(0);
}
#[test]
fn proc_root_received_trailing_no_creation() {
    proc_root_path_case(1);
}
#[test]
fn proc_root_received_plain_no_creation() {
    proc_root_path_case(2);
}
#[test]
fn proc_root_direct_trailing_no_creation() {
    proc_root_path_case(3);
}
#[test]
fn proc_root_real_root_trailing_positive() {
    proc_root_path_case(4);
}
#[test]
fn proc_root_direct_parent_exit_positive() {
    proc_root_path_case(5);
}

const PROC_ROOT_PATH_PROGRAM: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char cwd[4096], relative[8192], target[8192];
    if (!getcwd(cwd, sizeof(cwd)) || cwd[0] != '/' || strlen(cwd) < 20) return 80;
    if (snprintf(target, sizeof(target), "%s/private-created", cwd) >= sizeof(target)) return 81;
    struct stat initial;
    errno = 0;
    if (lstat(target, &initial) == 0 || errno != ENOENT) return 82;
    int descriptor = open(CASE == 0 ? cwd : (CASE == 4 ? "/" : "/proc"), O_RDONLY | O_DIRECTORY);
    if (descriptor < 0) return 83;
    if (CASE == 1 || CASE == 2) {
        int sockets[2];
        if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sockets)) return 84;
        char payload = 'x';
        char control[CMSG_SPACE(sizeof(int))] = {0};
        struct iovec vector = {.iov_base = &payload, .iov_len = 1};
        struct msghdr message = {.msg_iov = &vector, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof(control)};
        struct cmsghdr *header = CMSG_FIRSTHDR(&message);
        header->cmsg_level = SOL_SOCKET;
        header->cmsg_type = SCM_RIGHTS;
        header->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(header), &descriptor, sizeof(int));
        if (sendmsg(sockets[0], &message, 0) != 1 || close(descriptor)) return 85;
        memset(control, 0, sizeof(control));
        message.msg_controllen = sizeof(control);
        payload = 0;
        if (recvmsg(sockets[1], &message, 0) != 1 || payload != 'x' || (message.msg_flags & MSG_CTRUNC)) return 86;
        header = CMSG_FIRSTHDR(&message);
        if (!header || header->cmsg_level != SOL_SOCKET || header->cmsg_type != SCM_RIGHTS || header->cmsg_len != CMSG_LEN(sizeof(int))) return 87;
        memcpy(&descriptor, CMSG_DATA(header), sizeof(int));
        close(sockets[0]);
        close(sockets[1]);
    }
    if (CASE == 0) {
        if (snprintf(relative, sizeof(relative), "private-created///") >= sizeof(relative)) return 88;
    } else {
        if (snprintf(relative, sizeof(relative), "%s%s/private-created%s", CASE == 5 ? "../" : "", cwd + 1, (CASE == 2 || CASE == 5) ? "" : "///") >= sizeof(relative)) return 89;
    }
    errno = 0;
    int result = mkdirat(descriptor, relative, 0700);
    int saved_errno = errno;
    struct stat after;
    errno = 0;
    int exists = lstat(target, &after) == 0;
    int lookup_errno = errno;
    if (!exists && lookup_errno != ENOENT) return 90;
    printf("case=%d result=%d errno=%d exists=%d directory=%d\n", CASE, result, saved_errno, exists, exists && S_ISDIR(after.st_mode));
    close(descriptor);
    int expected_creation = CASE == 0 || CASE == 4 || CASE == 5;
    int correct = expected_creation ? (result == 0 && exists && S_ISDIR(after.st_mode)) : (result == -1 && !exists);
    if (exists && rmdir(target)) return 91;
    return correct ? 0 : 94;
}
"#;

static NEXT_TEST_EXECUTABLE: AtomicU64 = AtomicU64::new(0);

struct TestExecutable(PathBuf);

impl TestExecutable {
    fn new(image: &[u8]) -> Self {
        let id = NEXT_TEST_EXECUTABLE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("reverie-kvm-exec-{}-{id}", std::process::id()));
        std::fs::write(&path, image).unwrap();
        Self(path)
    }
}

impl Drop for TestExecutable {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).unwrap();
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let id = NEXT_TEST_EXECUTABLE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("reverie-kvm-coreutils-{}-{id}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run_host_program_captured(
    program: &str,
    argv: &[&str],
    cwd: &std::path::Path,
) -> (Vec<u8>, Vec<u8>) {
    const REAL_PROGRAM_MEMORY_SIZE: usize = 256 * 1024 * 1024;

    let image = std::fs::read(program).unwrap();
    let mut backend = KvmBackend::new(REAL_PROGRAM_MEMORY_SIZE).unwrap();
    backend
        .install_static_elf_with_context(&image, argv, &["PATH=/usr/bin:/bin"], cwd)
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(
        code,
        0,
        "{program} {argv:?} exited {code}; stdout={}; stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
    (stdout, stderr)
}

fn run_host_program_with_tool_captured(
    program: &str,
    argv: &[&str],
    cwd: &std::path::Path,
) -> (Vec<u8>, Vec<u8>) {
    const REAL_PROGRAM_MEMORY_SIZE: usize = 256 * 1024 * 1024;

    let image = std::fs::read(program).unwrap();
    let mut backend = KvmBackend::new(REAL_PROGRAM_MEMORY_SIZE).unwrap();
    backend
        .install_static_elf_with_context(&image, argv, &["PATH=/usr/bin:/bin"], cwd)
        .unwrap();
    let (_, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();
    assert_eq!(
        code,
        0,
        "{program} {argv:?} exited {code}; stdout={}; stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
    (stdout, stderr)
}

fn run_host_program(program: &str, argv: &[&str], cwd: &std::path::Path) {
    let _ = run_host_program_captured(program, argv, cwd);
}

fn compile_c_program(directory: &std::path::Path, name: &str, source: &str) -> PathBuf {
    compile_c_program_with_args(directory, name, source, &[])
}

fn compile_c_program_with_args(
    directory: &std::path::Path,
    name: &str,
    source: &str,
    extra_args: &[&str],
) -> PathBuf {
    let source_path = directory.join(format!("{name}.c"));
    let executable_path = directory.join(name);
    std::fs::write(&source_path, source).unwrap();
    let output = std::process::Command::new("/usr/bin/gcc")
        .args(["-O2", "-pthread"])
        .args(extra_args)
        .arg(&source_path)
        .arg("-o")
        .arg(&executable_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "gcc failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    executable_path
}

fn set_interrupt_signal_blocked(blocked: bool) -> bool {
    // SAFETY: set and previous are initialized before libc reads or writes them.
    unsafe {
        let mut set = std::mem::zeroed::<libc::sigset_t>();
        let mut previous = std::mem::zeroed::<libc::sigset_t>();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGURG);
        let how = if blocked {
            libc::SIG_BLOCK
        } else {
            libc::SIG_UNBLOCK
        };
        assert_eq!(libc::pthread_sigmask(how, &set, &mut previous), 0);
        libc::sigismember(&previous, libc::SIGURG) == 1
    }
}

#[derive(Default)]
struct PostExecLog {
    at_random: Mutex<Option<usize>>,
    calls: AtomicU64,
}

impl PostExecLog {
    fn at_random(&self) -> Option<usize> {
        *self.at_random.lock().expect("post-exec log lock poisoned")
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[reverie::global_tool]
impl GlobalTool for PostExecLog {
    type Request = usize;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, at_random: usize) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.at_random.lock().expect("post-exec log lock poisoned") = Some(at_random);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PostExecTool;

#[reverie::tool]
impl Tool for PostExecTool {
    type GlobalState = PostExecLog;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::execve]);
        subscriptions
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        guest.tail_inject(syscall).await
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        let auxv = guest.auxv();
        let address = auxv.at_random().ok_or(Errno::EINVAL)?;
        guest.send_rpc(address.as_raw()).await;
        // This lifecycle hook runs before the ELF entry point, matching execve.
        let address = unsafe { address.into_mut() };
        guest.memory().write_value(address, &POST_EXEC_RANDOM)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CanonicalInitialExecTool;

#[reverie::tool]
impl Tool for CanonicalInitialExecTool {
    type GlobalState = PostExecLog;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::execve]);
        subscriptions
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let Syscall::Execve(execve) = syscall else {
            unreachable!("tool only subscribes to execve")
        };
        guest
            .tail_inject(reverie::syscalls::Execveat::from(execve))
            .await
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        guest.send_rpc(0).await;
        Ok(())
    }
}

#[derive(Default)]
struct StartExecLog {
    post_exec_calls: AtomicU64,
}

impl StartExecLog {
    fn post_exec_calls(&self) -> u64 {
        self.post_exec_calls.load(Ordering::SeqCst)
    }
}

#[reverie::global_tool]
impl GlobalTool for StartExecLog {
    type Request = ();
    type Response = ();
    type Config = (usize, usize, usize);

    async fn receive_rpc(&self, _from: Pid, (): ()) {
        self.post_exec_calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StartExecTool;

#[reverie::tool]
impl Tool for StartExecTool {
    type GlobalState = StartExecLog;
    type ThreadState = ();

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        let (path, argv, envp) = *guest.config();
        let execve = Execve::new()
            .with_path(PathPtr::from_ptr(path as *const libc::c_char))
            .with_argv(Option::<CArrayPtr<CStrPtr>>::from_raw(argv))
            .with_envp(Option::<CArrayPtr<CStrPtr>>::from_raw(envp));
        guest.inject(execve).await?;
        Err(Errno::EINVAL.into())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        guest.send_rpc(()).await;
        Ok(())
    }
}

#[derive(Default)]
struct RpcRoundTripLog {
    response_base: u64,
    requests: Mutex<Vec<(Pid, u64)>>,
}

impl RpcRoundTripLog {
    fn requests(&self) -> Vec<(Pid, u64)> {
        self.requests
            .lock()
            .expect("RPC round-trip log lock poisoned")
            .clone()
    }
}

#[reverie::global_tool]
impl GlobalTool for RpcRoundTripLog {
    type Request = u64;
    type Response = u64;
    type Config = u64;

    async fn init_global_state(response_base: &u64) -> Self {
        Self {
            response_base: *response_base,
            requests: Mutex::default(),
        }
    }

    async fn receive_rpc(&self, from: Pid, ordinal: u64) -> u64 {
        self.requests
            .lock()
            .expect("RPC round-trip log lock poisoned")
            .push((from, ordinal));
        self.response_base + ordinal
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RpcRoundTripTool;

#[reverie::tool]
impl Tool for RpcRoundTripTool {
    type GlobalState = RpcRoundTripLog;
    type ThreadState = u64;

    fn subscriptions(_config: &u64) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::getpid);
        subscriptions
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert!(matches!(syscall, Syscall::Getpid(_)));
        let ordinal = {
            let ordinal = guest.thread_state_mut();
            *ordinal += 1;
            *ordinal
        };
        Ok(guest.send_rpc(ordinal).await as i64)
    }

    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        _tid: Pid,
        global: &G,
        thread_state: Self::ThreadState,
        _status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        let ordinal = thread_state + 1;
        assert_eq!(global.send_rpc(ordinal).await, *global.config() + ordinal);
        Ok(())
    }
}

struct ConcurrentToolStackLog {
    rendezvous: Barrier,
    tids: Mutex<Vec<Pid>>,
}

impl Default for ConcurrentToolStackLog {
    fn default() -> Self {
        Self {
            rendezvous: Barrier::new(2),
            tids: Mutex::new(Vec::new()),
        }
    }
}

impl ConcurrentToolStackLog {
    fn tids(&self) -> Vec<Pid> {
        self.tids
            .lock()
            .expect("concurrent Tool stack log poisoned")
            .clone()
    }
}

#[reverie::global_tool]
impl GlobalTool for ConcurrentToolStackLog {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, from: Pid, (): ()) {
        self.tids
            .lock()
            .expect("concurrent Tool stack log poisoned")
            .push(from);
        self.rendezvous.wait();
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ConcurrentToolStackTool;

#[reverie::tool]
impl Tool for ConcurrentToolStackTool {
    type GlobalState = ConcurrentToolStackLog;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::getppid);
        subscriptions
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert!(matches!(syscall, Syscall::Getppid(_)));
        let expected = u64::try_from(guest.tid().as_raw()).unwrap();
        let mut stack = guest.stack().await;
        let address = stack.push(expected);
        let guard = stack.commit()?;
        guest.send_rpc(()).await;
        let observed = guest.memory().read_value(address)?;
        if observed != expected {
            guest.tail_inject(ExitGroup::new().with_status(91)).await
        }
        drop(guard);
        Ok(guest.inject(syscall).await?)
    }
}

struct WorkerExecOverlapLog {
    rendezvous: Barrier,
    worker_errno: Mutex<Option<i32>>,
    worker_ready: Condvar,
    root_value_preserved: AtomicBool,
}

impl Default for WorkerExecOverlapLog {
    fn default() -> Self {
        Self {
            rendezvous: Barrier::new(2),
            worker_errno: Mutex::new(None),
            worker_ready: Condvar::new(),
            root_value_preserved: AtomicBool::new(false),
        }
    }
}

impl WorkerExecOverlapLog {
    fn worker_errno(&self) -> Option<i32> {
        *self
            .worker_errno
            .lock()
            .expect("worker exec result lock poisoned")
    }

    fn root_value_preserved(&self) -> bool {
        self.root_value_preserved.load(Ordering::SeqCst)
    }
}

#[reverie::global_tool]
impl GlobalTool for WorkerExecOverlapLog {
    type Request = (u8, i64);
    type Response = i64;
    type Config = (usize, usize, usize, bool, i32);

    async fn receive_rpc(&self, _from: Pid, (operation, value): (u8, i64)) -> i64 {
        match operation {
            // Both callbacks hold committed stack guards before either
            // proceeds to the image-replacement attempt.
            0 => {
                self.rendezvous.wait();
                0
            }
            // Publish the worker's observed errno and wake the root callback.
            1 => {
                *self
                    .worker_errno
                    .lock()
                    .expect("worker exec result lock poisoned") =
                    Some(i32::try_from(value).expect("errno must fit i32"));
                self.worker_ready.notify_all();
                0
            }
            // Wait with a bound so the pre-fix successful replacement cannot
            // leave the test blocked indefinitely.
            2 => {
                let result = self
                    .worker_errno
                    .lock()
                    .expect("worker exec result lock poisoned");
                let (result, _) = self
                    .worker_ready
                    .wait_timeout_while(result, std::time::Duration::from_secs(5), |result| {
                        result.is_none()
                    })
                    .expect("worker exec result wait poisoned");
                result.map(i64::from).unwrap_or(-1)
            }
            // Record that the root callback could still read its committed
            // value after the worker's refused image replacement.
            3 => {
                self.root_value_preserved
                    .store(value != 0, Ordering::SeqCst);
                0
            }
            _ => panic!("unexpected worker exec test operation {operation}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WorkerExecOverlapTool;

#[reverie::tool]
impl Tool for WorkerExecOverlapTool {
    type GlobalState = WorkerExecOverlapLog;
    type ThreadState = ();

    fn subscriptions(_config: &(usize, usize, usize, bool, i32)) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::getppid);
        subscriptions
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert!(matches!(syscall, Syscall::Getppid(_)));
        let expected = u64::try_from(guest.tid().as_raw()).unwrap();
        let mut stack = guest.stack().await;
        let address = stack.push(expected);
        let guard = stack.commit()?;
        guest.send_rpc((0, 0)).await;

        if guest.is_main_thread() {
            let worker_errno = guest.send_rpc((2, 0)).await;
            let observed = guest.memory().read_value(address)?;
            let preserved = worker_errno == i64::from(guest.config().4) && observed == expected;
            guest.send_rpc((3, i64::from(preserved))).await;
            if !preserved {
                guest.tail_inject(ExitGroup::new().with_status(92)).await
            }
        } else {
            assert_ne!(guest.tid(), guest.pid());
            let (path, argv, envp, execveat, _) = *guest.config();
            let request = Execve::new()
                .with_path(PathPtr::from_ptr(path as *const libc::c_char))
                .with_argv(Option::<CArrayPtr<CStrPtr>>::from_raw(argv))
                .with_envp(Option::<CArrayPtr<CStrPtr>>::from_raw(envp));
            let result = if execveat {
                guest
                    .inject(reverie::syscalls::Execveat::from(request))
                    .await
            } else {
                guest.inject(request).await
            };
            let error = result.expect_err("worker image replacement unexpectedly succeeded");
            guest.send_rpc((1, i64::from(error.into_raw()))).await;
        }

        drop(guard);
        Ok(guest.inject(syscall).await?)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DoubleForkTool;

#[reverie::tool]
impl Tool for DoubleForkTool {
    type GlobalState = ();
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::fork]);
        subscriptions
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let Syscall::Fork(fork) = syscall else {
            panic!("expected fork, got {syscall:?}");
        };
        let first = guest.inject(fork).await?;
        let second = guest.inject(Fork::new()).await?;
        assert!(first > 0);
        assert!(second > first);
        Ok(first)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FailingPostExecTool;

#[reverie::tool]
impl Tool for FailingPostExecTool {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_post_exec<G: Guest<Self>>(&self, _guest: &mut G) -> Result<(), Errno> {
        Err(Errno::EINVAL)
    }

    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        _tid: Pid,
        _global: &G,
        _thread_state: Self::ThreadState,
        _status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        POST_EXEC_FAILURE_EXITED.store(true, Ordering::SeqCst);
        Ok(())
    }
}

fn kvm_is_unavailable(error: &kvm_ioctls::Error) -> bool {
    matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM)
}

fn kvm_available(test: &str) -> bool {
    match Kvm::new() {
        Ok(_) => true,
        Err(error) if kvm_is_unavailable(&error) => {
            if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                panic!("{test} requires usable /dev/kvm: {error}");
            }
            eprintln!("skipping {test}: cannot open /dev/kvm: {error}");
            false
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }
}

fn assert_invalid_opcode(error: Error) {
    match error {
        Error::GuestException {
            vector,
            instruction_pointer,
            ..
        } => {
            assert_eq!(vector, 6);
            assert_eq!(instruction_pointer, LOAD_ADDRESS);
        }
        error => panic!("expected invalid-opcode exception, got {error}"),
    }
}

fn assert_page_fault(error: Error) {
    match error {
        Error::GuestException {
            vector,
            instruction_pointer,
            fault_address,
        } => {
            assert_eq!(vector, 14);
            assert_eq!(instruction_pointer, LOAD_ADDRESS);
            assert_eq!(fault_address, 0x4000_0000);
        }
        error => panic!("expected page-fault exception, got {error}"),
    }
}

fn assert_general_protection(error: Error) {
    match error {
        Error::GuestException {
            vector,
            instruction_pointer,
            ..
        } => {
            assert_eq!(vector, 13);
            assert_eq!(instruction_pointer, LOAD_ADDRESS);
        }
        error => panic!("expected general-protection exception, got {error}"),
    }
}

#[test]
fn static_elf_faults_are_reported_by_direct_and_tool_runtimes() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM exception test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let image = static_elf(&[0x0f, 0x0b]);

    let mut direct_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    direct_backend
        .install_static_elf(&image, "/bin/fault")
        .unwrap();
    assert_invalid_opcode(direct_backend.run_static_elf().unwrap_err());

    let mut tool_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    tool_backend
        .install_static_elf(&image, "/bin/fault")
        .unwrap();
    let error = match futures::executor::block_on(
        tool_backend.run_static_elf_with_tool::<StraceTool>((), true),
    ) {
        Ok(_) => panic!("tool runtime reported a guest exception as success"),
        Err(error) => error,
    };
    assert_invalid_opcode(error);

    // movabs rax, qword ptr [0x40000000], an address outside the page tables.
    let page_fault_image =
        static_elf(&[0x48, 0xa1, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00]);
    let mut page_fault_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    page_fault_backend
        .install_static_elf(&page_fault_image, "/bin/fault")
        .unwrap();
    assert_page_fault(page_fault_backend.run_static_elf().unwrap_err());

    let mut io_fault_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    io_fault_backend
        .install_static_elf(&static_elf(&[0xed]), "/bin/fault")
        .unwrap();
    assert_general_protection(io_fault_backend.run_static_elf().unwrap_err());
}

#[test]
fn static_elf_vmware_probe_reports_non_vmware_in_direct_and_tool_runtimes() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM VMware probe test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let code = [
        0xbb, 0x68, 0x58, 0x4d, 0x56, // mov ebx, 0x564d5868
        0xb9, 0x58, 0x56, 0x00, 0x00, // mov ecx, 0x5658
        0x31, 0xd2, // xor edx, edx
        0xed, // in eax, dx
        0x85, 0xdb, // test ebx, ebx
        0x75, 0x09, // jne failure
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0xb8, 0x3c, 0x00, 0x00, 0x00, // failure: mov eax, SYS_exit
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1
        0x0f, 0x05, // syscall
    ];
    let image = static_elf(&code);

    let mut direct_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    direct_backend
        .install_static_elf(&image, "/bin/vmware-probe")
        .unwrap();
    assert_eq!(direct_backend.run_static_elf().unwrap(), 0);

    let mut tool_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    tool_backend
        .install_static_elf(&image, "/bin/vmware-probe")
        .unwrap();
    let (_, code, stdout, stderr) =
        futures::executor::block_on(tool_backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn static_elf_cannot_copy_supervisor_bootstrap_memory() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM bootstrap access test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let code = [
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1
        0xbe, 0x00, 0x10, 0x00, 0x00, // mov esi, 0x1000
        0xba, 0x10, 0x00, 0x00, 0x00, // mov edx, 16
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, SYS_write
        0x0f, 0x05, // syscall
        0x48, 0x83, 0xf8, 0xf2, // cmp rax, -EFAULT
        0x74, 0x0e, // je success
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0xbf, 0x2a, 0x00, 0x00, 0x00, // mov edi, 42
        0x0f, 0x05, 0x0f, 0x0b, // syscall; ud2
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, 0x0f, 0x0b, // syscall; ud2
    ];

    for with_tool in [false, true] {
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_static_elf(&static_elf(&code), "/bin/bootstrap-access-test")
            .unwrap();
        let (exit_code, stdout, stderr) = if with_tool {
            let (_, exit_code, stdout, stderr) = futures::executor::block_on(
                backend.run_static_elf_with_tool::<StraceTool>((), true),
            )
            .unwrap();
            (exit_code, stdout, stderr)
        } else {
            backend.run_static_elf_captured().unwrap()
        };
        assert_eq!(exit_code, 0, "with_tool={with_tool}");
        assert!(stdout.is_empty(), "with_tool={with_tool}");
        assert!(stderr.is_empty(), "with_tool={with_tool}");
    }
}

#[test]
fn static_elf_forks_execs_and_waits_for_child() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM multiprocess test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let message = b"hello from fork exec\n";
    let mut target = vec![0xbf, 0x01, 0x00, 0x00, 0x00]; // mov edi, 1
    let message_operand = target.len() + 2;
    target.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, message
    target.push(0xba);
    target.extend_from_slice(&(message.len() as u32).to_le_bytes()); // mov edx, len
    target.extend_from_slice(&[0xb8, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x05]); // write
    target.extend_from_slice(&[
        0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xff, 0x0f, 0x05, 0x0f, 0x0b,
    ]); // exit_group(0); ud2
    let message_address = LOAD_ADDRESS + target.len() as u64;
    target[message_operand..message_operand + 8].copy_from_slice(&message_address.to_le_bytes());
    target.extend_from_slice(message);
    let executable = TestExecutable::new(&static_elf(&target));
    let path = executable.0.to_str().unwrap().as_bytes();

    let mut root = vec![
        0x49, 0xc7, 0xc4, 0x78, 0x56, 0x34, 0x12, // mov r12, 0x12345678
        0xb8, 0x78, 0x56, 0x34, 0x12, // mov eax, 0x12345678
        0x66, 0x0f, 0x6e, 0xc0, // movd xmm0, eax
        0xb8, 0x39, 0x00, 0x00, 0x00, // mov eax, SYS_fork
        0x0f, 0x05, // syscall
        0x85, 0xc0, // test eax, eax
        0x74, 0x00, // jz child
    ];
    let child_jump = root.len() - 1;
    root.extend_from_slice(&[
        0x89, 0xc7, // mov edi, eax
        0x48, 0x83, 0xec, 0x10, // sub rsp, 16
        0x48, 0x89, 0xe6, // mov rsi, rsp
        0x31, 0xd2, // xor edx, edx
        0x45, 0x31, 0xd2, // xor r10d, r10d
        0xb8, 0x3d, 0x00, 0x00, 0x00, // mov eax, SYS_wait4
        0x0f, 0x05, // syscall
        0x8b, 0x3c, 0x24, // mov edi, dword ptr [rsp]
        0xc1, 0xef, 0x08, // shr edi, 8
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ]);
    let child_offset = root.len();
    let displacement = child_offset as isize - (child_jump + 1) as isize;
    root[child_jump] = i8::try_from(displacement).unwrap() as u8;

    root.extend_from_slice(&[
        0x49, 0x81, 0xfc, 0x78, 0x56, 0x34, 0x12, // cmp r12, 0x12345678
        0x74, 0x0e, // je callee_saved_ok
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0xbf, 0x2a, 0x00, 0x00, 0x00, // mov edi, 42
        0x0f, 0x05, 0x0f, 0x0b, // syscall; ud2
        0x66, 0x0f, 0x7e, 0xc0, // movd eax, xmm0
        0x3d, 0x78, 0x56, 0x34, 0x12, // cmp eax, 0x12345678
        0x74, 0x0e, // je fpu_ok
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0xbf, 0x2b, 0x00, 0x00, 0x00, // mov edi, 43
        0x0f, 0x05, 0x0f, 0x0b, // syscall; ud2
    ]);

    let path_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbf, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdi, path
    let argv_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, argv
    let envp_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xba, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdx, envp
    root.extend_from_slice(&[
        0xb8, 0x3b, 0x00, 0x00, 0x00, 0x0f, 0x05, // execve
        0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x2a, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x0f,
        0x0b, // exit_group(42); ud2
    ]);

    let path_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(path);
    root.push(0);
    while !root.len().is_multiple_of(8) {
        root.push(0);
    }
    let argv_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&path_address.to_le_bytes());
    root.extend_from_slice(&0_u64.to_le_bytes());
    let envp_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&0_u64.to_le_bytes());
    root[path_operand..path_operand + 8].copy_from_slice(&path_address.to_le_bytes());
    root[argv_operand..argv_operand + 8].copy_from_slice(&argv_address.to_le_bytes());
    root[envp_operand..envp_operand + 8].copy_from_slice(&envp_address.to_le_bytes());

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&root), "/bin/fork-exec-test")
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();

    assert_eq!(code, 0);
    assert_eq!(stdout, message);
    assert!(stderr.is_empty());

    let mut tool_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    tool_backend
        .install_static_elf(&static_elf(&root), "/bin/fork-exec-test")
        .unwrap();
    let (_, code, stdout, stderr) =
        futures::executor::block_on(tool_backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();

    assert_eq!(code, 0);
    assert_eq!(stdout, message);
    assert!(stderr.is_empty());
}

#[test]
fn static_elf_self_abort_terminates_instead_of_faulting() {
    // Regression: glibc abort() writes its diagnostic, then raises SIGABRT via
    // tgkill(pid, tid, SIGABRT). Previously SIGABRT was unhandled (ENOSYS), so
    // abort() fell through to its "unreachable" hlt trap and the VM reported a
    // spurious #GP (exception vector 13). A self-directed fatal signal must now
    // terminate the process with the conventional 128 + signo status while
    // preserving output emitted before the signal.
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM self-abort test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let message = b"before abort\n";
    // write(1, message, len)
    let mut code = vec![0xbf, 0x01, 0x00, 0x00, 0x00]; // mov edi, 1
    let message_operand = code.len() + 2;
    code.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, message
    code.push(0xba);
    code.extend_from_slice(&(message.len() as u32).to_le_bytes()); // mov edx, len
    code.extend_from_slice(&[0xb8, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x05]); // mov eax, SYS_write; syscall
    // pid = getpid(); tgkill(pid, pid, SIGABRT)
    code.extend_from_slice(&[
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, SYS_getpid
        0x0f, 0x05, // syscall -> rax = pid
        0x89, 0xc7, // mov edi, eax  (tgid)
        0x89, 0xc6, // mov esi, eax  (tid)
        0xba, 0x06, 0x00, 0x00, 0x00, // mov edx, SIGABRT
        0xb8, 0xea, 0x00, 0x00, 0x00, // mov eax, SYS_tgkill
        0x0f, 0x05, // syscall -> must terminate here
        0x0f, 0x0b, // ud2 (only reached if the signal did not terminate us)
    ]);
    let message_address = LOAD_ADDRESS + code.len() as u64;
    code[message_operand..message_operand + 8].copy_from_slice(&message_address.to_le_bytes());
    code.extend_from_slice(message);

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&code), "/bin/self-abort")
        .unwrap();
    let (code_result, stdout, stderr) = backend.run_static_elf_captured().unwrap();

    // 128 + SIGABRT(6) == 134, matching the shell/native convention.
    assert_eq!(code_result, 128 + libc::SIGABRT);
    assert_eq!(stdout, message);
    assert!(stderr.is_empty());
}

#[test]
fn static_elf_clone_tid_side_effects_reach_guest_memory() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM clone TID test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    fn append_exit(code: &mut Vec<u8>, status: u32) {
        code.extend_from_slice(&[0xb8, 0xe7, 0x00, 0x00, 0x00]);
        code.push(0xbf);
        code.extend_from_slice(&status.to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x0f, 0x0b]);
    }

    fn patch_jump(code: &mut [u8], operand: usize, target: usize) {
        let displacement = i32::try_from(target as isize - (operand + 4) as isize).unwrap();
        code[operand..operand + 4].copy_from_slice(&displacement.to_le_bytes());
    }

    const PARENT_TID: u64 = LOAD_ADDRESS + 0x1800;
    const CHILD_TID: u64 = LOAD_ADDRESS + 0x1808;
    const REPLACEMENT_CLEAR_TID: u64 = LOAD_ADDRESS + 0x1810;
    const INVALID_TID: u64 = MEMORY_SIZE as u64 - 1;
    let flags = libc::SIGCHLD as u32
        | libc::CLONE_PARENT_SETTID as u32
        | libc::CLONE_CHILD_SETTID as u32
        | libc::CLONE_CHILD_CLEARTID as u32;

    let mut code = Vec::new();
    code.extend_from_slice(&[0xb8, 0x38, 0x00, 0x00, 0x00]); // mov eax, SYS_clone
    code.push(0xbf); // mov edi, flags
    code.extend_from_slice(&flags.to_le_bytes());
    code.extend_from_slice(&[0x31, 0xf6]); // xor esi, esi
    code.extend_from_slice(&[0x48, 0xba]); // movabs rdx, parent_tid
    code.extend_from_slice(&PARENT_TID.to_le_bytes());
    code.extend_from_slice(&[0x49, 0xba]); // movabs r10, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    code.extend_from_slice(&[0x0f, 0x05, 0x85, 0xc0, 0x0f, 0x84, 0x00, 0x00, 0x00, 0x00]); // syscall; jz child
    let first_child_jump = code.len() - 4;

    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, parent_tid
    code.extend_from_slice(&PARENT_TID.to_le_bytes());
    code.extend_from_slice(&[0x39, 0x01, 0x74, 0x0e]); // cmp [rcx], eax; je parent_tid_ok
    append_exit(&mut code, 61);
    code.extend_from_slice(&[
        0x89, 0xc7, // mov edi, eax
        0x48, 0x83, 0xec, 0x10, // sub rsp, 16
        0x48, 0x89, 0xe6, // mov rsi, rsp
        0x31, 0xd2, // xor edx, edx
        0x45, 0x31, 0xd2, // xor r10d, r10d
        0xb8, 0x3d, 0x00, 0x00, 0x00, // mov eax, SYS_wait4
        0x0f, 0x05, // syscall
        0x83, 0x3c, 0x24, 0x00, // cmp dword ptr [rsp], 0
        0x74, 0x0e, // je first_child_ok
    ]);
    append_exit(&mut code, 64);

    // A second clone proves invalid TID stores do not abort child creation.
    code.extend_from_slice(&[0xb8, 0x38, 0x00, 0x00, 0x00]);
    code.push(0xbf);
    code.extend_from_slice(&flags.to_le_bytes());
    code.extend_from_slice(&[0x31, 0xf6]); // xor esi, esi
    code.extend_from_slice(&[0x48, 0xba]); // movabs rdx, invalid parent_tid
    code.extend_from_slice(&INVALID_TID.to_le_bytes());
    code.extend_from_slice(&[0x49, 0xba]); // movabs r10, invalid child_tid
    code.extend_from_slice(&INVALID_TID.to_le_bytes());
    code.extend_from_slice(&[
        0x0f, 0x05, // syscall
        0x85, 0xc0, // test eax, eax
        0x79, 0x0e, // jns clone_returned_pid_or_child
    ]);
    append_exit(&mut code, 65);
    code.extend_from_slice(&[0x0f, 0x84, 0x00, 0x00, 0x00, 0x00]); // jz child
    let invalid_child_jump = code.len() - 4;
    code.extend_from_slice(&[
        0x89, 0xc7, // mov edi, eax
        0x48, 0x89, 0xe6, // mov rsi, rsp
        0x31, 0xd2, // xor edx, edx
        0x45, 0x31, 0xd2, // xor r10d, r10d
        0xb8, 0x3d, 0x00, 0x00, 0x00, // mov eax, SYS_wait4
        0x0f, 0x05, // syscall
        0x39, 0xf8, // cmp eax, edi
        0x74, 0x0e, // je waited_for_second_child
    ]);
    append_exit(&mut code, 66);
    code.extend_from_slice(&[
        0x8b, 0x3c, 0x24, // mov edi, dword ptr [rsp]
        0xc1, 0xef, 0x08, // shr edi, 8
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x0f, 0x05, 0x0f, 0x0b, // syscall; ud2
    ]);

    let first_child = code.len();
    patch_jump(&mut code, first_child_jump, first_child);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    code.extend_from_slice(&[0x83, 0x39, 0x02, 0x74, 0x0e]); // cmp [rcx], 2; je
    append_exit(&mut code, 62);
    code.extend_from_slice(&[0xb8, 0xda, 0x00, 0x00, 0x00]); // set_tid_address
    code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, replacement pointer
    code.extend_from_slice(&REPLACEMENT_CLEAR_TID.to_le_bytes());
    code.extend_from_slice(&[0x0f, 0x05, 0x83, 0xf8, 0x02, 0x74, 0x0e]); // syscall; cmp eax, 2; je
    append_exit(&mut code, 63);
    append_exit(&mut code, 0);

    let invalid_store_child = code.len();
    patch_jump(&mut code, invalid_child_jump, invalid_store_child);
    append_exit(&mut code, 0);

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&code), "/bin/clone-tid-test")
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn static_elf_runs_glibc_clone3_thread_and_restores_parent_state() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM clone3 thread test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    fn append_exit(code: &mut Vec<u8>, status: u32) {
        code.extend_from_slice(&[0xb8, 0xe7, 0x00, 0x00, 0x00]);
        code.push(0xbf);
        code.extend_from_slice(&status.to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x0f, 0x0b]);
    }

    fn patch_jump(code: &mut [u8], operand: usize, target: usize) {
        let displacement = i32::try_from(target as isize - (operand + 4) as isize).unwrap();
        code[operand..operand + 4].copy_from_slice(&displacement.to_le_bytes());
    }

    const PARENT_TID: u64 = LOAD_ADDRESS + 0x1800;
    const CHILD_TID: u64 = LOAD_ADDRESS + 0x1808;
    const CHILD_RESULT: u64 = LOAD_ADDRESS + 0x1810;
    const CHILD_FS: u64 = LOAD_ADDRESS + 0x1818;
    const CHILD_RSP: u64 = LOAD_ADDRESS + 0x1820;
    const TLS: u64 = LOAD_ADDRESS + 0x1880;
    const CHILD_STACK: u64 = LOAD_ADDRESS + 0x1900;
    const CHILD_STACK_SIZE: u64 = 0x600;
    const CHILD_STACK_TOP: u64 = CHILD_STACK + CHILD_STACK_SIZE;
    let flags = libc::CLONE_VM as u64
        | libc::CLONE_FS as u64
        | libc::CLONE_FILES as u64
        | libc::CLONE_SIGHAND as u64
        | libc::CLONE_THREAD as u64
        | libc::CLONE_SYSVSEM as u64
        | libc::CLONE_SETTLS as u64
        | libc::CLONE_PARENT_SETTID as u64
        | libc::CLONE_CHILD_CLEARTID as u64;

    let mut code = vec![
        0x49, 0x89, 0xe4, // mov r12, rsp
        0xb8, 0x78, 0x56, 0x34, 0x12, // mov eax, 0x12345678
        0x66, 0x0f, 0x6e, 0xc0, // movd xmm0, eax
    ];
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    code.extend_from_slice(&[0xc7, 0x01, 0xff, 0xff, 0xff, 0x7f]); // mov [rcx], sentinel
    code.extend_from_slice(&[0xb8, 0xb3, 0x01, 0x00, 0x00]); // mov eax, SYS_clone3
    let clone_args_operand = code.len() + 2;
    code.extend_from_slice(&[0x48, 0xbf, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdi, clone_args
    code.extend_from_slice(&[0xbe, 0x58, 0x00, 0x00, 0x00]); // mov esi, sizeof(clone_args)
    code.extend_from_slice(&[0x0f, 0x05, 0x85, 0xc0, 0x0f, 0x84, 0, 0, 0, 0]); // syscall; jz child
    let child_jump = code.len() - 4;

    code.extend_from_slice(&[0x4c, 0x39, 0xe4, 0x74, 0x0e]); // cmp rsp, r12; je
    append_exit(&mut code, 81);
    code.extend_from_slice(&[0x41, 0x89, 0xc5]); // mov r13d, eax
    code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    code.extend_from_slice(&[
        0xbe, 0x00, 0x00, 0x00, 0x00, // mov esi, FUTEX_WAIT
        0x44, 0x89, 0xea, // mov edx, r13d
        0x45, 0x31, 0xd2, // xor r10d, r10d
        0xb8, 0xca, 0x00, 0x00, 0x00, // mov eax, SYS_futex
        0x0f, 0x05, // syscall
        0x83, 0x3f, 0x00, // cmp dword ptr [rdi], 0
        0x75, 0xe9, // jne FUTEX_WAIT
        0x44, 0x89, 0xe8, // mov eax, r13d
    ]);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, parent_tid
    code.extend_from_slice(&PARENT_TID.to_le_bytes());
    code.extend_from_slice(&[0x39, 0x01, 0x74, 0x0e]); // cmp [rcx], eax; je
    append_exit(&mut code, 82);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_result
    code.extend_from_slice(&CHILD_RESULT.to_le_bytes());
    code.extend_from_slice(&[0x39, 0x01, 0x74, 0x0e]); // cmp [rcx], eax; je
    append_exit(&mut code, 83);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    code.extend_from_slice(&[0x83, 0x39, 0x00, 0x74, 0x0e]); // cmp dword ptr [rcx], 0; je
    append_exit(&mut code, 84);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_fs
    code.extend_from_slice(&CHILD_FS.to_le_bytes());
    code.extend_from_slice(&[0x48, 0xba]); // movabs rdx, tls
    code.extend_from_slice(&TLS.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x39, 0x11, 0x74, 0x0e]); // cmp [rcx], rdx; je
    append_exit(&mut code, 85);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_rsp
    code.extend_from_slice(&CHILD_RSP.to_le_bytes());
    code.extend_from_slice(&[0x48, 0xba]); // movabs rdx, child_stack_top
    code.extend_from_slice(&CHILD_STACK_TOP.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x39, 0x11, 0x74, 0x0e]); // cmp [rcx], rdx; je
    append_exit(&mut code, 86);
    code.extend_from_slice(&[
        0x66, 0x0f, 0x7e, 0xc0, // movd eax, xmm0
        0x3d, 0x78, 0x56, 0x34, 0x12, // cmp eax, 0x12345678
        0x74, 0x0e, // je
    ]);
    append_exit(&mut code, 87);
    code.extend_from_slice(&[
        0xb8, 0xba, 0x00, 0x00, 0x00, // mov eax, SYS_gettid
        0x0f, 0x05, // syscall
        0x83, 0xf8, 0x01, // cmp eax, 1
        0x74, 0x0e, // je
    ]);
    append_exit(&mut code, 88);
    append_exit(&mut code, 0);

    let child = code.len();
    patch_jump(&mut code, child_jump, child);
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_rsp
    code.extend_from_slice(&CHILD_RSP.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x89, 0x21]); // mov [rcx], rsp
    code.extend_from_slice(&[0xb8, 0x9e, 0x00, 0x00, 0x00]); // mov eax, SYS_arch_prctl
    code.extend_from_slice(&[0xbf, 0x03, 0x10, 0x00, 0x00]); // mov edi, ARCH_GET_FS
    code.extend_from_slice(&[0x48, 0xbe]); // movabs rsi, child_fs
    code.extend_from_slice(&CHILD_FS.to_le_bytes());
    code.extend_from_slice(&[0x0f, 0x05]); // syscall
    code.extend_from_slice(&[0xb8, 0xba, 0x00, 0x00, 0x00, 0x0f, 0x05]); // gettid
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_result
    code.extend_from_slice(&CHILD_RESULT.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x01]); // mov [rcx], eax
    code.extend_from_slice(&[
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ]);

    while !code.len().is_multiple_of(8) {
        code.push(0);
    }
    let clone_args_address = LOAD_ADDRESS + code.len() as u64;
    code[clone_args_operand..clone_args_operand + 8]
        .copy_from_slice(&clone_args_address.to_le_bytes());
    let mut clone_args = [0_u8; 88];
    clone_args[0..8].copy_from_slice(&flags.to_le_bytes());
    // Linux ignores pidfd without CLONE_PIDFD; glibc aliases this union slot.
    clone_args[8..16].copy_from_slice(&CHILD_TID.to_le_bytes());
    clone_args[16..24].copy_from_slice(&CHILD_TID.to_le_bytes());
    clone_args[24..32].copy_from_slice(&PARENT_TID.to_le_bytes());
    clone_args[40..48].copy_from_slice(&CHILD_STACK.to_le_bytes());
    clone_args[48..56].copy_from_slice(&CHILD_STACK_SIZE.to_le_bytes());
    clone_args[56..64].copy_from_slice(&TLS.to_le_bytes());
    code.extend_from_slice(&clone_args);

    // Exercise every worker-dispatch path so a `pthread_join`-style
    // `FUTEX_WAIT`/`CLEARTID` round trip completes (exit 0, no hang) in each:
    //   * `Direct`: the non-Tool personality (`run_process_action`).
    //   * `ToolDefault`: run a tool with *no* explicit ownership override, so the
    //     backend resolves ownership from `Tool::thread_ownership` — whose
    //     default is Tool-owned "follow children". This locks in the safe
    //     default (worker on the Tool loop, `futex` routed to the Tool) so a KVM
    //     caller no longer has to opt threads in.
    //   * `Tool(ThreadOwnership::Host)`: force the hybrid model, where
    //     `run_process_action_with_tool` falls through to the direct worker path
    //     and `futex` stays host-owned. Both execution and futex ownership are
    //     Host, so the round trip is consistent and cannot deadlock.
    //   * `Tool(ThreadOwnership::Tool)`: force the worker onto the Tool loop with
    //     `futex` routed to the Tool.
    #[derive(Debug, Clone, Copy)]
    enum WorkerDispatch {
        Direct,
        ToolDefault,
        Tool(ThreadOwnership),
    }
    for dispatch in [
        WorkerDispatch::Direct,
        WorkerDispatch::ToolDefault,
        WorkerDispatch::Tool(ThreadOwnership::Host),
        WorkerDispatch::Tool(ThreadOwnership::Tool),
    ] {
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_static_elf(&static_elf(&code), "/bin/clone3-thread-test")
            .unwrap();
        let (exit_code, stdout, stderr) = match dispatch {
            WorkerDispatch::Direct => backend.run_static_elf_captured().unwrap(),
            WorkerDispatch::ToolDefault => {
                // No set_thread_ownership: rely on the resolved default.
                let (_, exit_code, stdout, stderr) = futures::executor::block_on(
                    backend.run_static_elf_with_tool::<StraceTool>((), true),
                )
                .unwrap();
                (exit_code, stdout, stderr)
            }
            WorkerDispatch::Tool(ownership) => {
                backend.set_thread_ownership(ownership);
                let (_, exit_code, stdout, stderr) = futures::executor::block_on(
                    backend.run_static_elf_with_tool::<StraceTool>((), true),
                )
                .unwrap();
                (exit_code, stdout, stderr)
            }
        };
        assert_eq!(exit_code, 0, "dispatch={dispatch:?}");
        assert!(stdout.is_empty(), "dispatch={dispatch:?}");
        assert!(stderr.is_empty(), "dispatch={dispatch:?}");
    }
}

#[test]
fn host_owned_worker_descriptors_keep_read_and_readv_backend_owned() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM Host-owned descriptor test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "host-owned-worker-descriptors",
        r#"
#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <sys/eventfd.h>
#include <sys/uio.h>
#include <unistd.h>

#define EVENT_READ_FD 198
#define VECTOR_READ_FD 199

static _Atomic int ready;

static void *worker(void *unused) {
  (void)unused;
  int event = eventfd(0, EFD_CLOEXEC);
  int pipe_fds[2];
  if (event < 0 || pipe(pipe_fds) != 0 ||
      dup2(event, EVENT_READ_FD) != EVENT_READ_FD ||
      dup2(pipe_fds[0], VECTOR_READ_FD) != VECTOR_READ_FD) {
    atomic_store_explicit(&ready, -1, memory_order_release);
    return (void *)(uintptr_t)1;
  }
  close(event);
  close(pipe_fds[0]);

  uint64_t counter = 7;
  char first = 'v';
  char second = 'r';
  struct iovec vector[2] = {
      {.iov_base = &first, .iov_len = 1},
      {.iov_base = &second, .iov_len = 1},
  };
  if (write(EVENT_READ_FD, &counter, sizeof(counter)) != sizeof(counter) ||
      writev(pipe_fds[1], vector, 2) != 2) {
    atomic_store_explicit(&ready, -1, memory_order_release);
    return (void *)(uintptr_t)1;
  }
  close(pipe_fds[1]);
  atomic_store_explicit(&ready, 1, memory_order_release);
  return NULL;
}

int main(void) {
  pthread_t thread;
  if (pthread_create(&thread, NULL, worker, NULL) != 0) {
    return 10;
  }
  int state;
  while ((state = atomic_load_explicit(&ready, memory_order_acquire)) == 0) {
  }
  if (state < 0) {
    return 11;
  }

  uint64_t counter = 0;
  if (read(EVENT_READ_FD, &counter, sizeof(counter)) != sizeof(counter) || counter != 7) {
    return 12;
  }
  char first = 0;
  char second = 0;
  struct iovec vector[2] = {
      {.iov_base = &first, .iov_len = 1},
      {.iov_base = &second, .iov_len = 1},
  };
  if (readv(VECTOR_READ_FD, vector, 2) != 2 || first != 'v' || second != 'r') {
    return 13;
  }

  void *result = NULL;
  if (pthread_join(thread, &result) != 0 || result != NULL) {
    return 14;
  }
  return 0;
}
"#,
    );
    let executable = executable.to_str().unwrap();
    let image = std::fs::read(executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    backend.set_thread_ownership(ThreadOwnership::Host);
    let (trace, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let entries = trace.formatted();
    assert!(
        !entries.iter().any(|entry| entry.starts_with("read(198,")),
        "Host-owned eventfd read unexpectedly reached the Tool: {entries:?}"
    );
    assert!(
        !entries.iter().any(|entry| entry.starts_with("readv(199,")),
        "Host-owned pipe readv unexpectedly reached the Tool: {entries:?}"
    );
}

#[test]
fn real_glibc_get_robust_list_tracks_fork_and_thread_lifecycles() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM robust-list lifecycle test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "robust-list-lifecycle",
        r#"
#define _GNU_SOURCE
#include <errno.h>
#include <linux/futex.h>
#include <pthread.h>
#include <stdio.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int tid_pipe[2];
static int release_pipe[2];

static void *worker(void *unused) {
  (void)unused;
  pid_t tid = (pid_t)syscall(SYS_gettid);
  struct robust_list_head *self_head = NULL;
  struct robust_list_head *leader_head = NULL;
  size_t self_len = 0;
  size_t leader_len = 0;
  char byte = 0;

  if (syscall(SYS_get_robust_list, 0, &self_head, &self_len) != 0 ||
      syscall(SYS_get_robust_list, getpid(), &leader_head, &leader_len) != 0 ||
      self_head == NULL || leader_head == NULL || self_head == leader_head ||
      self_len != sizeof(*self_head) || leader_len != sizeof(*leader_head) ||
      write(tid_pipe[1], &tid, sizeof(tid)) != sizeof(tid) ||
      read(release_pipe[0], &byte, sizeof(byte)) != sizeof(byte)) {
    return (void *)(uintptr_t)1;
  }
  return NULL;
}

int main(void) {
  int ready_pipe[2];
  int child_release_pipe[2];
  if (pipe(ready_pipe) != 0 || pipe(child_release_pipe) != 0) {
    return 10;
  }

  pid_t child = fork();
  if (child < 0) {
    return 11;
  }
  if (child == 0) {
    char byte = 1;
    if (write(ready_pipe[1], &byte, sizeof(byte)) != sizeof(byte) ||
        read(child_release_pipe[0], &byte, sizeof(byte)) != sizeof(byte)) {
      _exit(12);
    }
    _exit(0);
  }

  char byte = 0;
  if (read(ready_pipe[0], &byte, sizeof(byte)) != sizeof(byte)) {
    return 13;
  }
  struct robust_list_head *head = (void *)(uintptr_t)1;
  size_t length = 0;
  if (syscall(SYS_get_robust_list, child, &head, &length) != 0 ||
      length != sizeof(*head)) {
    return 14;
  }
  byte = 1;
  if (write(child_release_pipe[1], &byte, sizeof(byte)) != sizeof(byte)) {
    return 15;
  }
  int status = 0;
  if (waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
      WEXITSTATUS(status) != 0) {
    return 16;
  }
  errno = 0;
  if (syscall(SYS_get_robust_list, child, &head, &length) != -1 ||
      errno != ESRCH) {
    return 17;
  }

  if (pipe(tid_pipe) != 0 || pipe(release_pipe) != 0) {
    return 18;
  }
  pthread_t thread;
  if (pthread_create(&thread, NULL, worker, NULL) != 0) {
    return 19;
  }
  pid_t tid = 0;
  if (read(tid_pipe[0], &tid, sizeof(tid)) != sizeof(tid)) {
    return 20;
  }
  head = NULL;
  length = 0;
  if (syscall(SYS_get_robust_list, tid, &head, &length) != 0 ||
      head == NULL || length != sizeof(*head)) {
    return 21;
  }
  byte = 1;
  if (write(release_pipe[1], &byte, sizeof(byte)) != sizeof(byte)) {
    return 22;
  }
  void *result = NULL;
  if (pthread_join(thread, &result) != 0 || result != NULL) {
    return 23;
  }
  errno = 0;
  if (syscall(SYS_get_robust_list, tid, &head, &length) != -1 ||
      errno != ESRCH) {
    return 24;
  }

  puts("robust-list lifecycle ok");
  return 0;
}
"#,
    );
    let executable = executable.to_str().unwrap();
    let (stdout, stderr) =
        run_host_program_with_tool_captured(executable, &[executable], &directory.0);
    assert_eq!(stdout, b"robust-list lifecycle ok\n");
    assert!(
        stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&stderr)
    );
}

#[test]
fn real_glibc_scm_rights_translate_across_thread_and_fork_tables() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM SCM_RIGHTS test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "scm-rights-translation",
        r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static int sockets[2];
static int received_fds[2] = {-1, -1};

static void *receive_rights(void *unused) {
  (void)unused;
  char payload = 0;
  char control[CMSG_SPACE(2 * sizeof(int))];
  struct iovec iov = {.iov_base = &payload, .iov_len = 1};
  struct msghdr message;
  memset(&message, 0, sizeof(message));
  memset(control, 0, sizeof(control));
  message.msg_iov = &iov;
  message.msg_iovlen = 1;
  message.msg_control = control;
  message.msg_controllen = sizeof(control);
  if (recvmsg(sockets[1], &message, MSG_CMSG_CLOEXEC) != 1 || payload != 'q' ||
      (message.msg_flags & MSG_CTRUNC) != 0) {
    return (void *)1;
  }
  struct cmsghdr *cmsg = CMSG_FIRSTHDR(&message);
  if (cmsg == NULL || cmsg->cmsg_level != SOL_SOCKET ||
      cmsg->cmsg_type != SCM_RIGHTS ||
      cmsg->cmsg_len != CMSG_LEN(2 * sizeof(int))) {
    return (void *)2;
  }
  memcpy(received_fds, CMSG_DATA(cmsg), sizeof(received_fds));
  return NULL;
}

static int send_rights(const int fds[2]) {
  char payload = 'q';
  char control[CMSG_SPACE(2 * sizeof(int))];
  struct iovec iov = {.iov_base = &payload, .iov_len = 1};
  struct msghdr message;
  memset(&message, 0, sizeof(message));
  memset(control, 0, sizeof(control));
  message.msg_iov = &iov;
  message.msg_iovlen = 1;
  message.msg_control = control;
  message.msg_controllen = sizeof(control);
  struct cmsghdr *cmsg = CMSG_FIRSTHDR(&message);
  cmsg->cmsg_level = SOL_SOCKET;
  cmsg->cmsg_type = SCM_RIGHTS;
  cmsg->cmsg_len = CMSG_LEN(2 * sizeof(int));
  memcpy(CMSG_DATA(cmsg), fds, 2 * sizeof(int));
  return (int)sendmsg(sockets[0], &message, 0);
}

int main(void) {
  int pipe_fds[2];
  if (socketpair(AF_UNIX, SOCK_DGRAM, 0, sockets) != 0 || pipe(pipe_fds) != 0 ||
      sockets[0] != 3 || sockets[1] != 4 || pipe_fds[0] != 5 || pipe_fds[1] != 6) {
    return 10;
  }

  int invalid[2] = {pipe_fds[0], 999};
  errno = 0;
  if (send_rights(invalid) != -1 || errno != EBADF) {
    return 11;
  }
  if (send_rights(pipe_fds) != 1) {
    return 12;
  }

  pthread_t thread;
  if (pthread_create(&thread, NULL, receive_rights, NULL) != 0) {
    return 13;
  }
  void *thread_result = NULL;
  if (pthread_join(thread, &thread_result) != 0 || thread_result != NULL) {
    return 14;
  }
  if (received_fds[0] != 7 || received_fds[1] != 8 ||
      (fcntl(received_fds[0], F_GETFD) & FD_CLOEXEC) == 0 ||
      (fcntl(received_fds[1], F_GETFD) & FD_CLOEXEC) == 0) {
    return 15;
  }

  close(pipe_fds[0]);
  close(pipe_fds[1]);
  pid_t child = fork();
  if (child < 0) {
    return 16;
  }
  if (child == 0) {
    char byte = 'z';
    _exit(write(received_fds[1], &byte, 1) == 1 ? 0 : 17);
  }
  char byte = 0;
  int status = 0;
  if (read(received_fds[0], &byte, 1) != 1 || byte != 'z' ||
      waitpid(child, &status, 0) != child || !WIFEXITED(status) ||
      WEXITSTATUS(status) != 0) {
    return 18;
  }

  puts("scm-rights translation ok");
  return 0;
}
"#,
    );
    let executable = executable.to_str().unwrap();
    let (stdout, stderr) =
        run_host_program_with_tool_captured(executable, &[executable], &directory.0);
    assert_eq!(stdout, b"scm-rights translation ok\n");
    assert!(
        stderr.is_empty(),
        "stderr={}",
        String::from_utf8_lossy(&stderr)
    );
}

#[test]
fn worker_exit_group_terminates_the_root_with_its_status() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM worker exit_group test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    fn append_exit_group(code: &mut Vec<u8>, status: u32) {
        code.extend_from_slice(&[0xb8, 0xe7, 0x00, 0x00, 0x00]);
        code.push(0xbf);
        code.extend_from_slice(&status.to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x0f, 0x0b]);
    }

    fn patch_jump(code: &mut [u8], operand: usize, target: usize) {
        let displacement = i32::try_from(target as isize - (operand + 4) as isize).unwrap();
        code[operand..operand + 4].copy_from_slice(&displacement.to_le_bytes());
    }

    const CHILD_TID: u64 = LOAD_ADDRESS + 0x1800;
    const CHILD_STACK: u64 = LOAD_ADDRESS + 0x1900;
    const CHILD_STACK_SIZE: u64 = 0x600;
    let flags = libc::CLONE_VM as u64
        | libc::CLONE_FS as u64
        | libc::CLONE_FILES as u64
        | libc::CLONE_SIGHAND as u64
        | libc::CLONE_THREAD as u64
        | libc::CLONE_CHILD_SETTID as u64
        | libc::CLONE_CHILD_CLEARTID as u64;

    let mut code = Vec::new();
    code.extend_from_slice(&[0xb8, 0xb3, 0x01, 0x00, 0x00]); // mov eax, SYS_clone3
    let clone_args_operand = code.len() + 2;
    code.extend_from_slice(&[0x48, 0xbf, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdi, clone_args
    code.extend_from_slice(&[0xbe, 0x58, 0x00, 0x00, 0x00]); // mov esi, sizeof(clone_args)
    code.extend_from_slice(&[0x0f, 0x05, 0x85, 0xc0, 0x0f, 0x84, 0, 0, 0, 0]); // syscall; jz child
    let child_jump = code.len() - 4;

    code.extend_from_slice(&[0x41, 0x89, 0xc5]); // mov r13d, eax
    code.extend_from_slice(&[0x48, 0xbf]); // movabs rdi, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    let wait = code.len();
    code.extend_from_slice(&[
        0xbe, 0x00, 0x00, 0x00, 0x00, // mov esi, FUTEX_WAIT
        0x44, 0x89, 0xea, // mov edx, r13d
        0x45, 0x31, 0xd2, // xor r10d, r10d
        0xb8, 0xca, 0x00, 0x00, 0x00, // mov eax, SYS_futex
        0x0f, 0x05, // syscall
        0x83, 0x3f, 0x00, // cmp dword ptr [rdi], 0
        0x0f, 0x85, 0, 0, 0, 0, // jne wait
    ]);
    let wait_jump = code.len() - 4;
    patch_jump(&mut code, wait_jump, wait);
    append_exit_group(&mut code, 0);

    let child = code.len();
    patch_jump(&mut code, child_jump, child);
    append_exit_group(&mut code, 37);

    while !code.len().is_multiple_of(8) {
        code.push(0);
    }
    let clone_args_address = LOAD_ADDRESS + code.len() as u64;
    code[clone_args_operand..clone_args_operand + 8]
        .copy_from_slice(&clone_args_address.to_le_bytes());
    let mut clone_args = [0_u8; 88];
    clone_args[0..8].copy_from_slice(&flags.to_le_bytes());
    clone_args[16..24].copy_from_slice(&CHILD_TID.to_le_bytes());
    clone_args[40..48].copy_from_slice(&CHILD_STACK.to_le_bytes());
    clone_args[48..56].copy_from_slice(&CHILD_STACK_SIZE.to_le_bytes());
    code.extend_from_slice(&clone_args);

    for with_tool in [false, true] {
        let was_blocked = set_interrupt_signal_blocked(true);
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_static_elf(&static_elf(&code), "/bin/worker-exit-group-test")
            .unwrap();
        let exit_code = if with_tool {
            let (_, exit_code, stdout, stderr) = futures::executor::block_on(
                backend.run_static_elf_with_tool::<StraceTool>((), true),
            )
            .unwrap();
            assert!(stdout.is_empty());
            assert!(stderr.is_empty());
            exit_code
        } else {
            backend.run_static_elf().unwrap()
        };
        let mut child_tid = [0; std::mem::size_of::<i32>()];
        backend.memory().read(CHILD_TID, &mut child_tid).unwrap();
        let remained_blocked = set_interrupt_signal_blocked(was_blocked);
        assert!(remained_blocked, "with_tool={with_tool}");
        assert_eq!(exit_code, 37, "with_tool={with_tool}");
        assert_eq!(i32::from_le_bytes(child_tid), 0, "with_tool={with_tool}");
    }
}

#[test]
fn real_bash_redirects_builtin_output_through_f_dupfd() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM Bash redirection test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let root = TestDirectory::new();
    let (stdout, stderr) = run_host_program_captured(
        "/bin/bash",
        &[
            "bash",
            "--norc",
            "-c",
            "printf redirected > output; printf visible",
        ],
        &root.0,
    );
    assert_eq!(stdout, b"visible");
    assert!(stderr.is_empty());
    assert_eq!(std::fs::read(root.0.join("output")).unwrap(), b"redirected");
}

#[test]
fn real_bash_small_pipeline_uses_legacy_process_clone_tid_flags() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM Bash pipeline test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let root = TestDirectory::new();
    // The child runs to completion before the parent resumes, so this covers a
    // bounded pipeline without claiming concurrent producer/consumer support.
    let (stdout, stderr) = run_host_program_captured(
        "/bin/bash",
        &["bash", "--norc", "-c", "printf abc | /usr/bin/wc -c"],
        &root.0,
    );
    assert_eq!(stdout, b"3\n");
    assert!(
        stderr.is_empty(),
        "unexpected Bash stderr: {}",
        String::from_utf8_lossy(&stderr)
    );
}

#[test]
fn real_coreutils_complete_file_mutation_workflow() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM coreutils test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let root = TestDirectory::new();
    std::fs::write(root.0.join("source"), b"payload\n").unwrap();

    run_host_program("/bin/mkdir", &["mkdir", "-p", "directory/nested"], &root.0);
    run_host_program("/usr/bin/touch", &["touch", "touched"], &root.0);
    run_host_program("/bin/chmod", &["chmod", "600", "touched"], &root.0);
    run_host_program("/bin/ln", &["ln", "source", "hard-link"], &root.0);
    run_host_program("/bin/ln", &["ln", "-s", "source", "symbolic-link"], &root.0);
    run_host_program("/bin/mv", &["mv", "hard-link", "renamed"], &root.0);
    run_host_program("/usr/bin/mkfifo", &["mkfifo", "fifo"], &root.0);
    run_host_program(
        "/usr/bin/install",
        &["install", "-m", "700", "source", "installed"],
        &root.0,
    );
    run_host_program("/bin/rm", &["rm", "renamed"], &root.0);
    run_host_program("/bin/rmdir", &["rmdir", "directory/nested"], &root.0);

    assert!(root.0.join("directory").is_dir());
    assert!(!root.0.join("directory/nested").exists());
    assert_eq!(std::fs::read(root.0.join("source")).unwrap(), b"payload\n");
    assert!(root.0.join("touched").is_file());
    assert_eq!(
        std::fs::read_link(root.0.join("symbolic-link")).unwrap(),
        std::path::Path::new("source")
    );
    assert!(
        std::fs::symlink_metadata(root.0.join("fifo"))
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert_eq!(
        std::fs::read(root.0.join("installed")).unwrap(),
        b"payload\n"
    );
    assert!(!root.0.join("renamed").exists());
}

#[test]
fn static_elf_executes_syscall_and_exits() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM static ELF test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();

    backend
        .memory_mut()
        .write(LOAD_ADDRESS + 0x1000, &[0xff])
        .unwrap();

    // Check BSS and argc, then require deterministic getpid == 1 and preserved
    // RBX. Any loader or SYSCALL return-state error takes the exit_group(42)
    // path rather than producing a false pass.
    let code = [
        0x48, 0xb8, 0x00, 0x10, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, // mov rax, 0x201000
        0x80, 0x38, 0x00, // cmp byte ptr [rax], 0
        0x75, 0x2d, // jne failure
        0x48, 0x83, 0x3c, 0x24, 0x01, // cmp qword ptr [rsp], 1
        0x75, 0x26, // jne failure
        0xbb, 0x78, 0x56, 0x34, 0x12, // mov ebx, 0x12345678
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, SYS_getpid
        0x0f, 0x05, // syscall
        0x48, 0x83, 0xf8, 0x01, // cmp rax, 1
        0x75, 0x14, // jne failure
        0x48, 0x81, 0xfb, 0x78, 0x56, 0x34, 0x12, // cmp rbx, 0x12345678
        0x75, 0x0b, // jne failure
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
        0xb8, 0xe7, 0x00, 0x00, 0x00, // failure: mov eax, SYS_exit_group
        0xbf, 0x2a, 0x00, 0x00, 0x00, // mov edi, 42
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    backend
        .install_static_elf(&static_elf(&code), "/bin/true")
        .unwrap();

    assert_eq!(backend.run_static_elf().unwrap(), 0);
}

#[test]
fn static_elf_tool_capture_preserves_configured_stdin() {
    if !kvm_available("KVM captured stdin test") {
        return;
    }

    let directory = TestDirectory::new();
    let stdin_path = directory.0.join("stdin");
    std::fs::write(&stdin_path, b"stdin-through-kvm\n").unwrap();
    let stdin = std::fs::File::open(stdin_path).unwrap();

    let image = std::fs::read("/bin/cat").unwrap();
    let mut backend = KvmBackend::new_with_stdin(256 * 1024 * 1024, Some(stdin)).unwrap();
    backend
        .install_static_elf_with_args(&image, &["/bin/cat"], &["PATH=/usr/bin:/bin"])
        .unwrap();

    let (_, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();

    assert_eq!(code, 0);
    assert_eq!(stdout, b"stdin-through-kvm\n");
    assert!(stderr.is_empty());

    let mut backend = KvmBackend::new_with_stdin(256 * 1024 * 1024, None).unwrap();
    backend
        .install_static_elf_with_args(&image, &["/bin/cat"], &["PATH=/usr/bin:/bin"])
        .unwrap();
    let (_, code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn kvm_initial_rbp_and_rflags_match_native_linux_process_entry() {
    if !kvm_available("KVM initial-register parity test") {
        return;
    }

    // Capture rbp and rflags before the guest makes its first syscall. The
    // temporary stack adjustment happens only after both entry values are in
    // callee-saved registers.
    let code = [
        0x9c, // pushfq
        0x5b, // pop rbx
        0x49, 0x89, 0xec, // mov r12, rbp
        0x48, 0x83, 0xec, 0x10, // sub rsp, 16
        0x4c, 0x89, 0x24, 0x24, // mov [rsp], r12
        0x48, 0x89, 0x5c, 0x24, 0x08, // mov [rsp + 8], rbx
        0xbf, 0x01, 0x00, 0x00, 0x00, // mov edi, 1
        0x48, 0x89, 0xe6, // mov rsi, rsp
        0xba, 0x10, 0x00, 0x00, 0x00, // mov edx, 16
        0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, SYS_write
        0x0f, 0x05, // syscall
        0xb8, 0x3c, 0x00, 0x00, 0x00, // mov eax, SYS_exit
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let image = static_elf(&code);
    let executable = TestExecutable::new(&image);
    let mut permissions = std::fs::metadata(&executable.0).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable.0, permissions).unwrap();

    let native = std::process::Command::new(&executable.0).output().unwrap();
    assert!(
        native.status.success(),
        "native entry-register fixture failed: {native:?}",
    );

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&image, "/bin/entry-registers")
        .unwrap();
    let (code, kvm_stdout, kvm_stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(
        code,
        0,
        "KVM entry-register fixture failed; stderr={}",
        String::from_utf8_lossy(&kvm_stderr),
    );

    let decode = |label: &str, bytes: &[u8]| {
        assert_eq!(bytes.len(), 16, "{label} emitted the wrong state size");
        let rbp = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let rflags = u64::from_le_bytes(bytes[8..].try_into().unwrap());
        (rbp, rflags)
    };
    let native_state = decode("native", &native.stdout);
    let kvm_state = decode("KVM", &kvm_stdout);

    assert_eq!(native_state.0, 0, "native Linux did not enter with rbp=0");
    assert_eq!(
        native_state.1, 0x202,
        "native Linux did not enter with reserved bit and IF set",
    );
    assert_eq!(
        kvm_state, native_state,
        "KVM must reproduce native Linux's observed rbp and rflags at _start",
    );
}

#[test]
fn kvm_static_elf_getppid_follows_pid_namespace_contract() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM getppid namespace test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    // A static ELF guest that issues getppid and self-checks the deterministic
    // parent PID. Linux PID-namespace semantics give a conventional root guest
    // (detcore ROOT_DETPID == 3) getppid() == 1, while namespace init (PID 1)
    // has getppid() == 0. KVM synthesizes the guest identity and must reproduce
    // those pinned semantics. Any mismatch takes the exit_group(42) path.
    #[rustfmt::skip]
    fn getppid_probe(expected_ppid: u8) -> [u8; 36] {
        [
            0xb8, 0x6e, 0x00, 0x00, 0x00,   // mov eax, SYS_getppid (110)
            0x0f, 0x05,                     // syscall
            0x48, 0x83, 0xf8, expected_ppid, // cmp rax, expected_ppid
            0x75, 0x09,                     // jne failure (skip the 9-byte success block)
            0xb8, 0xe7, 0x00, 0x00, 0x00,   // mov eax, SYS_exit_group (231)
            0x31, 0xff,                     // xor edi, edi
            0x0f, 0x05,                     // syscall  (exit_group(0))
            0xb8, 0xe7, 0x00, 0x00, 0x00,   // failure: mov eax, SYS_exit_group
            0xbf, 0x2a, 0x00, 0x00, 0x00,   // mov edi, 42
            0x0f, 0x05,                     // syscall  (exit_group(42))
            0x0f, 0x0b,                     // ud2
        ]
    }

    // Conventional root guest: detcore ROOT_DETPID == 3 => getppid() == 1.
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&getppid_probe(1)), "/bin/true")
        .unwrap();
    backend.set_root_pid(3).unwrap();
    assert_eq!(
        backend.run_static_elf().unwrap(),
        0,
        "root guest pid=3 must report getppid()==1 in the PID namespace"
    );

    // Namespace init edge case: a guest that is itself PID 1 has no parent.
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&getppid_probe(0)), "/bin/true")
        .unwrap();
    // Default root_pid is already 1; set it explicitly to document intent.
    backend.set_root_pid(1).unwrap();
    assert_eq!(
        backend.run_static_elf().unwrap(),
        0,
        "namespace-init guest pid=1 must report getppid()==0"
    );
}

fn run_stats_program(code: &[u8], name: &str, request: BackendStatsRequest) -> KvmBackendStats {
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend.set_backend_stats_request(request);
    backend.install_static_elf(&static_elf(code), name).unwrap();
    let (_, exit_code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();
    assert_eq!(exit_code, 0, "{name}");
    assert!(stdout.is_empty(), "{name}");
    assert!(stderr.is_empty(), "{name}");

    let snapshot = backend.backend_stats();
    assert_eq!(
        request.collect(&backend),
        request.is_enabled().then(|| snapshot.clone()),
        "{name} request and snapshot source must agree"
    );
    snapshot
}

fn stats_root_program() -> Vec<u8> {
    vec![
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, SYS_getpid
        0x0f, 0x05, // syscall
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ]
}

fn patch_stats_jump(code: &mut [u8], operand: usize, target: usize) {
    let displacement = i32::try_from(target as isize - (operand + 4) as isize).unwrap();
    code[operand..operand + 4].copy_from_slice(&displacement.to_le_bytes());
}

fn append_stats_exit(code: &mut Vec<u8>, group: bool) {
    let number = if group {
        libc::SYS_exit_group
    } else {
        libc::SYS_exit
    };
    code.push(0xb8); // mov eax, SYS_exit[_group]
    code.extend_from_slice(&(number as u32).to_le_bytes());
    code.extend_from_slice(&[
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ]);
}

fn stats_fork_program() -> Vec<u8> {
    let mut code = vec![
        0xb8, 0x39, 0x00, 0x00, 0x00, // mov eax, SYS_fork
        0x0f, 0x05, // syscall
        0x85, 0xc0, // test eax, eax
        0x0f, 0x84, 0, 0, 0, 0, // jz child
    ];
    let child_jump = code.len() - 4;

    code.extend_from_slice(&[
        0x89, 0xc7, // mov edi, eax
        0x48, 0x83, 0xec, 0x10, // sub rsp, 16
        0x48, 0x89, 0xe6, // mov rsi, rsp
        0x31, 0xd2, // xor edx, edx
        0x45, 0x31, 0xd2, // xor r10d, r10d
        0xb8, 0x3d, 0x00, 0x00, 0x00, // mov eax, SYS_wait4
        0x0f, 0x05, // syscall
    ]);
    append_stats_exit(&mut code, true);

    let child = code.len();
    patch_stats_jump(&mut code, child_jump, child);
    code.extend_from_slice(&[
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, SYS_getpid
        0x0f, 0x05, // syscall
    ]);
    append_stats_exit(&mut code, true);
    code
}

fn clone_thread_program(probe_tool_stacks: bool) -> Vec<u8> {
    const CHILD_TID: u64 = LOAD_ADDRESS + 0x1800;
    const CHILD_STACK: u64 = LOAD_ADDRESS + 0x1900;
    const CHILD_STACK_SIZE: u64 = 0x600;

    let flags = libc::CLONE_VM as u64
        | libc::CLONE_FS as u64
        | libc::CLONE_FILES as u64
        | libc::CLONE_SIGHAND as u64
        | libc::CLONE_THREAD as u64
        | libc::CLONE_SYSVSEM as u64
        | libc::CLONE_CHILD_SETTID as u64
        | libc::CLONE_CHILD_CLEARTID as u64;

    let mut code = vec![0x48, 0xb9]; // movabs rcx, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    code.extend_from_slice(&[
        0xc7, 0x01, 0xff, 0xff, 0xff, 0x7f, // mov dword ptr [rcx], 0x7fffffff
        0xb8, 0xb3, 0x01, 0x00, 0x00, // mov eax, SYS_clone3
        0x48, 0xbf, // movabs rdi, clone_args
    ]);
    let clone_args_operand = code.len();
    code.extend_from_slice(&0_u64.to_le_bytes());
    code.extend_from_slice(&[
        0xbe, 0x58, 0x00, 0x00, 0x00, // mov esi, sizeof(clone_args)
        0x0f, 0x05, // syscall
        0x85, 0xc0, // test eax, eax
        0x0f, 0x84, 0, 0, 0, 0, // jz child
    ]);
    let child_jump = code.len() - 4;

    if probe_tool_stacks {
        code.extend_from_slice(&[
            0xb8, 0x6e, 0x00, 0x00, 0x00, // mov eax, SYS_getppid
            0x0f, 0x05, // syscall
        ]);
    }
    code.extend_from_slice(&[0x48, 0xb9]); // movabs rcx, child_tid
    code.extend_from_slice(&CHILD_TID.to_le_bytes());
    let wait = code.len();
    code.extend_from_slice(&[
        0x83, 0x39, 0x00, // cmp dword ptr [rcx], 0
        0x0f, 0x85, 0, 0, 0, 0, // jne wait
    ]);
    let wait_jump = code.len() - 4;
    patch_stats_jump(&mut code, wait_jump, wait);
    append_stats_exit(&mut code, true);

    let child = code.len();
    patch_stats_jump(&mut code, child_jump, child);
    if probe_tool_stacks {
        code.extend_from_slice(&[
            0xb8, 0x6e, 0x00, 0x00, 0x00, // mov eax, SYS_getppid
            0x0f, 0x05, // syscall
        ]);
    }
    code.extend_from_slice(&[
        0xb8, 0xba, 0x00, 0x00, 0x00, // mov eax, SYS_gettid
        0x0f, 0x05, // syscall
    ]);
    append_stats_exit(&mut code, false);

    while !code.len().is_multiple_of(8) {
        code.push(0);
    }
    let clone_args_address = LOAD_ADDRESS + code.len() as u64;
    // The branch target is patched before the trailing clone arguments are
    // appended, so the optional parent/child probes cannot make it point into
    // the data block.
    code[clone_args_operand..clone_args_operand + 8]
        .copy_from_slice(&clone_args_address.to_le_bytes());
    let mut clone_args = [0_u8; 88];
    clone_args[0..8].copy_from_slice(&flags.to_le_bytes());
    clone_args[16..24].copy_from_slice(&CHILD_TID.to_le_bytes());
    clone_args[40..48].copy_from_slice(&CHILD_STACK.to_le_bytes());
    clone_args[48..56].copy_from_slice(&CHILD_STACK_SIZE.to_le_bytes());
    code.extend_from_slice(&clone_args);
    code
}

fn assert_exact_stats(snapshot: &KvmBackendStats, hypercalls: u64, halts: u64) {
    assert_eq!(snapshot.count(KvmExitReason::Hypercall), hypercalls);
    assert_eq!(snapshot.count(KvmExitReason::Hlt), halts);
    assert_eq!(snapshot.total_exits(), hypercalls + halts, "{snapshot}");
}

#[test]
fn kvm_stats_disabled_run_records_no_exits() {
    if !kvm_available("kvm_stats_disabled_run_records_no_exits") {
        return;
    }

    let snapshot = run_stats_program(
        &stats_root_program(),
        "/bin/kvm-stats-disabled",
        BackendStatsRequest::DISABLED,
    );
    assert_exact_stats(&snapshot, 0, 0);
}

#[test]
fn kvm_stats_root_production_loop_is_exact_and_repeatable() {
    if !kvm_available("kvm_stats_root_production_loop_is_exact_and_repeatable") {
        return;
    }

    let first = run_stats_program(
        &stats_root_program(),
        "/bin/kvm-stats-root",
        BackendStatsRequest::ENABLED,
    );
    let second = run_stats_program(
        &stats_root_program(),
        "/bin/kvm-stats-root",
        BackendStatsRequest::ENABLED,
    );
    assert_exact_stats(&first, 2, 0);
    assert_eq!(first, second);
}

#[test]
fn kvm_stats_fork_process_tree_is_exact_and_repeatable() {
    if !kvm_available("kvm_stats_fork_process_tree_is_exact_and_repeatable") {
        return;
    }

    let first = run_stats_program(
        &stats_fork_program(),
        "/bin/kvm-stats-fork",
        BackendStatsRequest::ENABLED,
    );
    let second = run_stats_program(
        &stats_fork_program(),
        "/bin/kvm-stats-fork",
        BackendStatsRequest::ENABLED,
    );
    // Root: fork + wait4 + exit_group. Child: getpid + exit_group.
    assert_exact_stats(&first, 5, 1);
    assert_eq!(first, second);
}

#[test]
fn kvm_stats_clone_thread_process_tree_is_exact_and_repeatable() {
    if !kvm_available("kvm_stats_clone_thread_process_tree_is_exact_and_repeatable") {
        return;
    }

    let first = run_stats_program(
        &clone_thread_program(false),
        "/bin/kvm-stats-thread",
        BackendStatsRequest::ENABLED,
    );
    let second = run_stats_program(
        &clone_thread_program(false),
        "/bin/kvm-stats-thread",
        BackendStatsRequest::ENABLED,
    );
    // Root: clone3 + exit_group. Child: gettid + exit. The parent waits in
    // guest memory for CLONE_CHILD_CLEARTID, so the child exit is counted first.
    assert_exact_stats(&first, 4, 1);
    assert_eq!(first, second);
}

#[test]
fn tool_owned_threads_keep_independent_stack_guards_live() {
    if !kvm_available("tool_owned_threads_keep_independent_stack_guards_live") {
        return;
    }

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(
            &static_elf(&clone_thread_program(true)),
            "/bin/tool-stack-threads",
        )
        .unwrap();
    let (log, code, stdout, stderr) = futures::executor::block_on(
        backend.run_static_elf_with_tool::<ConcurrentToolStackTool>((), true),
    )
    .unwrap();

    assert_eq!(code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    let tids = log.tids();
    assert_eq!(tids.len(), 2);
    assert_ne!(tids[0], tids[1]);
}

#[test]
fn worker_exec_is_refused_without_replacing_shared_memory() {
    if !kvm_available("worker_exec_is_refused_without_replacing_shared_memory") {
        return;
    }

    let target = [
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let executable = TestExecutable::new(&static_elf(&target));
    for execveat in [false, true] {
        check_worker_exec_preserves_shared_memory(&executable.0, None, execveat, libc::ENOSYS);
    }
}

#[test]
fn worker_exec_preflight_errors_preserve_shared_memory() {
    if !kvm_available("worker_exec_preflight_errors_preserve_shared_memory") {
        return;
    }

    let executable = TestExecutable::new(&static_elf(&[0x0f, 0x0b]));
    let malformed = TestExecutable::new(b"not an ELF image");
    let missing = executable.0.with_extension("missing");
    assert!(!missing.exists());
    for execveat in [false, true] {
        for index in 0..3 {
            check_worker_exec_preserves_shared_memory(
                &executable.0,
                Some(index),
                execveat,
                libc::EFAULT,
            );
        }
        check_worker_exec_preserves_shared_memory(&missing, None, execveat, libc::ENOENT);
        check_worker_exec_preserves_shared_memory(&malformed.0, None, execveat, libc::ENOEXEC);
    }
}

fn check_worker_exec_preserves_shared_memory(
    path: &std::path::Path,
    invalid_argument: Option<usize>,
    execveat: bool,
    expected_errno: i32,
) {
    eprintln!(
        "execveat={execveat} invalid_argument={invalid_argument:?} expected_errno={expected_errno}"
    );
    let path = path.to_str().unwrap().as_bytes();

    let mut code = clone_thread_program(true);
    let path_address = LOAD_ADDRESS + code.len() as u64;
    code.extend_from_slice(path);
    code.push(0);
    while !code.len().is_multiple_of(8) {
        code.push(0);
    }
    let argv_address = LOAD_ADDRESS + code.len() as u64;
    code.extend_from_slice(&path_address.to_le_bytes());
    code.extend_from_slice(&0_u64.to_le_bytes());
    let envp_address = LOAD_ADDRESS + code.len() as u64;
    code.extend_from_slice(&0_u64.to_le_bytes());

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(
            &static_elf(&code),
            "/bin/worker-exec-preserves-shared-memory",
        )
        .unwrap();
    let mut pointers = [
        path_address as usize,
        argv_address as usize,
        envp_address as usize,
    ];
    if let Some(index) = invalid_argument {
        pointers[index] = MEMORY_SIZE + 0x1000;
    }
    let [path, argv, envp] = pointers;
    let config = (path, argv, envp, execveat, expected_errno);
    let (log, exit_code, stdout, stderr) = futures::executor::block_on(
        backend.run_static_elf_with_tool::<WorkerExecOverlapTool>(config, true),
    )
    .unwrap();

    eprintln!(
        "worker_errno={:?} exit_code={exit_code}",
        log.worker_errno()
    );
    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(log.worker_errno(), Some(expected_errno));
    assert!(log.root_value_preserved());
}

#[test]
fn static_elf_executes_avx_instruction() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM AVX test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    // The host dynamic linker uses this vmovq encoding unconditionally. KVM's
    // userspace-only guest must initialize OSXSAVE and the YMM register state
    // that a Linux kernel would normally configure before entering userspace.
    let code = [
        0x31, 0xff, // xor edi, edi
        0xc4, 0xe1, 0xf9, 0x6e, 0xcf, // vmovq rdi, xmm1
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&code), "/bin/avx-probe")
        .unwrap();

    assert_eq!(backend.run_static_elf().unwrap(), 0);
}

#[test]
fn dynamic_c_guest_observes_indexed_xstate_cpuid_and_lazy_binding() {
    if !kvm_available("dynamic_c_guest_observes_indexed_xstate_cpuid_and_lazy_binding") {
        return;
    }

    let directory = TestDirectory::new();
    let executable = compile_c_program_with_args(
        &directory.0,
        "indexed-xstate-cpuid",
        r#"
#include <cpuid.h>
#include <stdint.h>
#include <stdio.h>

struct registers {
  uint32_t eax;
  uint32_t ebx;
  uint32_t ecx;
  uint32_t edx;
};

static struct registers cpuid_xstate(uint32_t subleaf) {
  struct registers result;
  __cpuid_count(0xd, subleaf, result.eax, result.ebx, result.ecx, result.edx);
  return result;
}

static int matches(struct registers actual, struct registers expected) {
  return actual.eax == expected.eax && actual.ebx == expected.ebx &&
         actual.ecx == expected.ecx && actual.edx == expected.edx;
}

int main(void) {
  struct registers xstate = cpuid_xstate(0);
  /* KVM recomputes subleaf 0 EBX from XCR0 and the host-supported component
     layout. With x87, SSE, and AVX enabled, the guest-visible size is 0x340. */
  if (xstate.eax != 0x00000007 || xstate.ebx != 0x00000340 ||
      xstate.ecx != 0x00000340 || xstate.edx != 0) {
    return 10;
  }

  static const struct {
    uint32_t subleaf;
    struct registers expected;
  } cases[] = {
      {1, {0, 0, 0, 0}},
      {2, {0x00000100, 0x00000240, 0, 0}},
      {17, {0, 0, 0, 0}},
      {18, {0, 0, 0, 0}},
      {19, {0, 0, 0, 0}},
  };

  for (uint32_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
    if (!matches(cpuid_xstate(cases[i].subleaf), cases[i].expected)) {
      return 11 + (int)i;
    }
  }

  /* The executable is linked for lazy binding, so this first puts call makes
     the dynamic linker resolve its PLT entry after all CPUID checks pass. */
  if (puts("indexed xstate cpuid ok") < 0) {
    return 20;
  }
  return 0;
}
"#,
        &["-fno-builtin", "-Wl,-z,lazy"],
    );
    let image = std::fs::read(&executable).unwrap();
    let elf = goblin::elf::Elf::parse(&image).unwrap();
    assert!(elf.interpreter.is_some(), "test guest must be dynamic");
    let dynamic = elf
        .dynamic
        .as_ref()
        .expect("test guest must have a dynamic section");
    assert!(
        dynamic
            .dyns
            .iter()
            .all(|entry| entry.d_tag != goblin::elf::dynamic::DT_BIND_NOW),
        "test guest must not contain DT_BIND_NOW"
    );
    assert_eq!(
        dynamic.info.flags & goblin::elf::dynamic::DF_BIND_NOW,
        0,
        "test guest must not contain DF_BIND_NOW"
    );
    assert_eq!(
        dynamic.info.flags_1 & goblin::elf::dynamic::DF_1_NOW,
        0,
        "test guest must not contain DF_1_NOW"
    );
    assert!(
        elf.pltrelocs.iter().any(|relocation| {
            elf.dynsyms
                .get(relocation.r_sym)
                .and_then(|symbol| elf.dynstrtab.get_at(symbol.st_name))
                == Some("puts")
        }),
        "test guest must call puts through a PLT relocation"
    );

    let executable = executable.to_str().unwrap();
    let (stdout, stderr) = run_host_program_captured(executable, &[executable], &directory.0);
    assert_eq!(stdout, b"indexed xstate cpuid ok\n");
    assert!(stderr.is_empty());
}

#[test]
fn static_elf_receives_argv_and_envp() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM argv/envp test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    // exit_group(42): the failure path taken by every self-check below. Exactly
    // 12 bytes, so each conditional jump that skips it uses rel8 = 0x0c.
    const FAIL: [u8; 12] = [
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0xbf, 0x2a, 0x00, 0x00, 0x00, // mov edi, 42
        0x0f, 0x05, // syscall
    ];

    // The guest verifies the System V initial stack that the loader built for
    // argv = ["prog", "second"], envp = ["FOO=bar"]:
    //   [rsp+0]=argc [rsp+8]=argv0 [rsp+16]=argv1 [rsp+24]=NULL
    //   [rsp+32]=envp0 [rsp+40]=NULL
    // Any mismatch takes exit_group(42); success prints and exit_group(0).
    let message = b"hello from kvm m1\n";
    let mut code: Vec<u8> = Vec::new();
    // argc == 2
    code.extend_from_slice(&[0x48, 0x83, 0x3c, 0x24, 0x02, 0x74, 0x0c]); // cmp qword[rsp],2; je +12
    code.extend_from_slice(&FAIL);
    // argv[1] != 0
    code.extend_from_slice(&[0x48, 0x8b, 0x44, 0x24, 0x10, 0x48, 0x85, 0xc0, 0x75, 0x0c]); // mov rax,[rsp+16]; test; jne +12
    code.extend_from_slice(&FAIL);
    // envp[0] != 0
    code.extend_from_slice(&[0x48, 0x8b, 0x44, 0x24, 0x20, 0x48, 0x85, 0xc0, 0x75, 0x0c]); // mov rax,[rsp+32]; test; jne +12
    code.extend_from_slice(&FAIL);
    // envp[1] == 0 (single environment entry, then the NULL terminator)
    code.extend_from_slice(&[0x48, 0x8b, 0x44, 0x24, 0x28, 0x48, 0x85, 0xc0, 0x74, 0x0c]); // mov rax,[rsp+40]; test; je +12
    code.extend_from_slice(&FAIL);
    // write(1, message, message.len())
    code.extend_from_slice(&[0xbf, 0x01, 0x00, 0x00, 0x00]); // mov edi, 1
    let movabs_operand = code.len() + 2;
    code.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, <message vaddr>
    code.push(0xba);
    code.extend_from_slice(&(message.len() as u32).to_le_bytes()); // mov edx, len
    code.extend_from_slice(&[0xb8, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x05]); // mov eax,SYS_write; syscall
    // exit_group(0)
    code.extend_from_slice(&[
        0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xff, 0x0f, 0x05, 0x0f, 0x0b,
    ]); // mov eax,231; xor edi,edi; syscall; ud2
    let message_offset = code.len();
    code.extend_from_slice(message);
    let message_vaddr = LOAD_ADDRESS + message_offset as u64;
    code[movabs_operand..movabs_operand + 8].copy_from_slice(&message_vaddr.to_le_bytes());

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf_with_args(&static_elf(&code), &["prog", "second"], &["FOO=bar"])
        .unwrap();

    assert_eq!(backend.run_static_elf().unwrap(), 0);
}

#[test]
fn tool_receives_post_exec_with_guest_auxv() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM post-exec test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let code = [
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf_with_args(&static_elf(&code), &["prog"], &[])
        .unwrap();

    let (log, exit_code, _, _) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<PostExecTool>((), true))
            .unwrap();

    assert_eq!(exit_code, 0);
    assert_eq!(log.calls(), 1);
    let address = log
        .at_random()
        .expect("post-exec hook did not observe AT_RANDOM");
    let mut random = [0; 16];
    backend.memory().read(address as u64, &mut random).unwrap();
    assert_eq!(random, POST_EXEC_RANDOM);
}

#[test]
fn canonical_initial_execveat_does_not_reload_installed_image() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM canonical initial exec test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let code = [
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let image = static_elf(&code);
    let executable = TestExecutable::new(&image);
    let executable = executable.0.to_str().unwrap();
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend.install_static_elf(&image, executable).unwrap();

    // A second exec of the same file clears the loaded segment before
    // reloading it. This address is in mapped BSS but outside the file-backed
    // bytes, so it distinguishes forwarding the synthetic initial exec from a
    // real reload.
    const SENTINEL_ADDRESS: u64 = LOAD_ADDRESS + 0x1000;
    const SENTINEL: [u8; 16] = *b"initial-exec-ok!";
    backend
        .memory_mut()
        .write(SENTINEL_ADDRESS, &SENTINEL)
        .unwrap();

    let (log, exit_code, stdout, stderr) = futures::executor::block_on(
        backend.run_static_elf_with_tool::<CanonicalInitialExecTool>((), true),
    )
    .unwrap();

    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(log.calls(), 1);
    let mut observed = [0; SENTINEL.len()];
    backend
        .memory()
        .read(SENTINEL_ADDRESS, &mut observed)
        .unwrap();
    assert_eq!(observed, SENTINEL);
}

#[test]
fn successful_exec_discards_bytes_outside_replacement_image() {
    if !kvm_available("KVM exec page-discard test") {
        return;
    }

    const SENTINEL_ADDRESS: u64 = LOAD_ADDRESS + 0x1800;
    const SENTINEL: [u8; 16] = *b"old-image-bytes!";

    // Load the replacement at a disjoint address so its segment loader cannot
    // overwrite the sentinel. The replacement directly reads the old image's
    // page through KVM's user identity map and requires the discard to make it
    // demand-zero.
    let mut target = vec![0x48, 0xb8]; // movabs rax, SENTINEL_ADDRESS
    target.extend_from_slice(&SENTINEL_ADDRESS.to_le_bytes());
    target.extend_from_slice(&[
        0x80, 0x38, 0x00, // cmp byte ptr [rax], 0
        0x75, 0x0b, // jne stale_memory
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
        0xb8, 0xe7, 0x00, 0x00, 0x00, // stale_memory: mov eax, SYS_exit_group
        0xbf, 0x2a, 0x00, 0x00, 0x00, // mov edi, 42
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ]);
    let executable = TestExecutable::new(&static_elf_at(&target, LOAD_ADDRESS + 0x4000));
    let path = executable.0.to_str().unwrap().as_bytes();

    let mut root = Vec::new();
    let path_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbf, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdi, path
    let argv_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, argv
    let envp_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xba, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdx, envp
    root.extend_from_slice(&[
        0xb8, 0x3b, 0x00, 0x00, 0x00, 0x0f, 0x05, // execve
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0xbf, 0x4d, 0x00, 0x00, 0x00, // mov edi, 77
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ]);
    let path_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(path);
    root.push(0);
    while !root.len().is_multiple_of(8) {
        root.push(0);
    }
    let argv_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&path_address.to_le_bytes());
    root.extend_from_slice(&0_u64.to_le_bytes());
    let envp_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&0_u64.to_le_bytes());
    root[path_operand..path_operand + 8].copy_from_slice(&path_address.to_le_bytes());
    root[argv_operand..argv_operand + 8].copy_from_slice(&argv_address.to_le_bytes());
    root[envp_operand..envp_operand + 8].copy_from_slice(&envp_address.to_le_bytes());

    for with_tool in [false, true] {
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_static_elf(&static_elf(&root), "/bin/memory-replacement-test")
            .unwrap();
        backend
            .memory_mut()
            .write(SENTINEL_ADDRESS, &SENTINEL)
            .unwrap();
        let (exit_code, stdout, stderr) = if with_tool {
            let (_, exit_code, stdout, stderr) = futures::executor::block_on(
                backend.run_static_elf_with_tool::<StraceTool>((), true),
            )
            .unwrap();
            (exit_code, stdout, stderr)
        } else {
            backend.run_static_elf_captured().unwrap()
        };

        assert_eq!(exit_code, 0, "with_tool={with_tool}");
        assert!(stdout.is_empty(), "with_tool={with_tool}");
        assert!(stderr.is_empty(), "with_tool={with_tool}");
    }
}

#[test]
fn tool_receives_post_exec_after_root_execve() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM post-exec replacement test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let target = [
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let executable = TestExecutable::new(&static_elf(&target));
    let path = executable.0.to_str().unwrap().as_bytes();

    let mut root = Vec::new();
    let path_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbf, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdi, path
    let argv_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, argv
    let envp_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xba, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdx, envp
    root.extend_from_slice(&[
        0xb8, 0x3b, 0x00, 0x00, 0x00, 0x0f, 0x05, // execve
        0xb8, 0xe7, 0x00, 0x00, 0x00, 0xbf, 0x2a, 0x00, 0x00, 0x00, 0x0f, 0x05, 0x0f,
        0x0b, // exit_group(42); ud2
    ]);

    let path_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(path);
    root.push(0);
    while !root.len().is_multiple_of(8) {
        root.push(0);
    }
    let argv_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&path_address.to_le_bytes());
    root.extend_from_slice(&0_u64.to_le_bytes());
    let envp_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&0_u64.to_le_bytes());
    root[path_operand..path_operand + 8].copy_from_slice(&path_address.to_le_bytes());
    root[argv_operand..argv_operand + 8].copy_from_slice(&argv_address.to_le_bytes());
    root[envp_operand..envp_operand + 8].copy_from_slice(&envp_address.to_le_bytes());

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&root), "/bin/root-exec-test")
        .unwrap();

    let (log, exit_code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<PostExecTool>((), true))
            .unwrap();

    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(log.calls(), 2);
    let address = log
        .at_random()
        .expect("replacement post-exec hook did not observe AT_RANDOM");
    let mut random = [0; POST_EXEC_RANDOM.len()];
    backend.memory().read(address as u64, &mut random).unwrap();
    assert_eq!(random, POST_EXEC_RANDOM);
}

#[test]
fn tool_executes_from_thread_start_before_initial_entry() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM thread-start exec test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let target = [
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x31, 0xff, // xor edi, edi
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let executable = TestExecutable::new(&static_elf(&target));
    let path = executable.0.to_str().unwrap().as_bytes();

    let mut root = vec![
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0xbf, 0x2a, 0x00, 0x00, 0x00, // mov edi, 42
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let path_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(path);
    root.push(0);
    while !root.len().is_multiple_of(8) {
        root.push(0);
    }
    let argv_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&path_address.to_le_bytes());
    root.extend_from_slice(&0_u64.to_le_bytes());
    let envp_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&0_u64.to_le_bytes());

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&root), "/bin/thread-start-exec-test")
        .unwrap();
    let config = (
        path_address as usize,
        argv_address as usize,
        envp_address as usize,
    );
    let (log, exit_code, stdout, stderr) = futures::executor::block_on(
        backend.run_static_elf_with_tool::<StartExecTool>(config, true),
    )
    .unwrap();

    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(log.post_exec_calls(), 1);
}

#[test]
fn regular_injected_forks_complete_before_return() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM regular fork injection test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let code = [
        0xb8, 0x39, 0x00, 0x00, 0x00, // mov eax, SYS_fork
        0x0f, 0x05, // syscall
        0x89, 0xc7, // mov edi, eax
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&code), "/bin/double-injected-fork-test")
        .unwrap();

    let (_, exit_code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<DoubleForkTool>((), true))
            .unwrap();

    assert_eq!(exit_code, 2);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn malformed_exec_is_rejected_during_preflight_without_resetting_image() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM malformed exec test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    // This guest-visible ENOEXEC is decided by ElfExecutor's isolated
    // preflight, before exec_process reaches its fatal point of no return.
    let executable = TestExecutable::new(b"not an ELF image");
    let path = executable.0.to_str().unwrap().as_bytes();
    let mut root = Vec::new();
    let path_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbf, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdi, path
    let argv_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, argv
    let envp_operand = root.len() + 2;
    root.extend_from_slice(&[0x48, 0xba, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rdx, envp
    root.extend_from_slice(&[
        0xb8, 0x3b, 0x00, 0x00, 0x00, 0x0f, 0x05, // execve
        0xf7, 0xd8, // neg eax
        0x89, 0xc7, // mov edi, eax
        0xb8, 0xe7, 0x00, 0x00, 0x00, 0x0f, 0x05, // exit_group(errno)
        0x0f, 0x0b, // ud2
    ]);
    let path_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(path);
    root.push(0);
    while !root.len().is_multiple_of(8) {
        root.push(0);
    }
    let argv_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&path_address.to_le_bytes());
    root.extend_from_slice(&0_u64.to_le_bytes());
    let envp_address = LOAD_ADDRESS + root.len() as u64;
    root.extend_from_slice(&0_u64.to_le_bytes());
    root[path_operand..path_operand + 8].copy_from_slice(&path_address.to_le_bytes());
    root[argv_operand..argv_operand + 8].copy_from_slice(&argv_address.to_le_bytes());
    root[envp_operand..envp_operand + 8].copy_from_slice(&envp_address.to_le_bytes());

    for with_tool in [false, true] {
        const SENTINEL_ADDRESS: u64 = LOAD_ADDRESS + 0x1800;
        const SENTINEL: [u8; 16] = *b"failed-exec-kept";
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_static_elf(&static_elf(&root), "/bin/malformed-exec-test")
            .unwrap();
        backend
            .memory_mut()
            .write(SENTINEL_ADDRESS, &SENTINEL)
            .unwrap();
        let original_memory = backend.memory().clone();
        let (exit_code, stdout, stderr) = if with_tool {
            let (_, exit_code, stdout, stderr) = futures::executor::block_on(
                backend.run_static_elf_with_tool::<StraceTool>((), true),
            )
            .unwrap();
            (exit_code, stdout, stderr)
        } else {
            backend.run_static_elf_captured().unwrap()
        };
        assert_eq!(exit_code, libc::ENOEXEC, "with_tool={with_tool}");
        assert!(stdout.is_empty(), "with_tool={with_tool}");
        assert!(stderr.is_empty(), "with_tool={with_tool}");
        let mut observed = [0; SENTINEL.len()];
        backend
            .memory()
            .read(SENTINEL_ADDRESS, &mut observed)
            .unwrap();
        assert_eq!(observed, SENTINEL, "with_tool={with_tool}");
        original_memory
            .read(SENTINEL_ADDRESS, &mut observed)
            .unwrap();
        assert_eq!(observed, SENTINEL, "with_tool={with_tool}");
    }
}

#[test]
fn post_exec_failure_runs_tool_exit_lifecycle() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM post-exec failure test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    POST_EXEC_FAILURE_EXITED.store(false, Ordering::SeqCst);
    let code = [
        0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xff, 0x0f, 0x05, 0x0f, 0x0b,
    ];
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf_with_args(&static_elf(&code), &["prog"], &[])
        .unwrap();

    let error = futures::executor::block_on(
        backend.run_static_elf_with_tool::<FailingPostExecTool>((), true),
    )
    .unwrap_err();

    assert!(error.to_string().contains("post-exec hook failed"));
    assert!(POST_EXEC_FAILURE_EXITED.load(Ordering::SeqCst));
}

#[test]
fn strace_tool_logs_syscalls_from_static_elf() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM strace-ELF test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    // A static ELF guest that issues getpid, write(1, "hi\n", 3), exit_group(0)
    // via real SYSCALL instructions. Each traps through the ring0 trampoline and
    // must be observed by StraceTool, whose tail_inject is serviced by the ELF
    // guest kernel (so getpid returns 1, the write prints, and exit_group ends
    // the run). The synthetic initial exec is delivered too, but this test tool
    // records only after injection and successful exec does not return.
    let message = b"hi\n";
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0xb8, 0x27, 0x00, 0x00, 0x00, 0x0f, 0x05]); // mov eax,SYS_getpid; syscall
    code.extend_from_slice(&[0xbf, 0x01, 0x00, 0x00, 0x00]); // mov edi, 1
    let movabs_operand = code.len() + 2;
    code.extend_from_slice(&[0x48, 0xbe, 0, 0, 0, 0, 0, 0, 0, 0]); // movabs rsi, <message vaddr>
    code.push(0xba);
    code.extend_from_slice(&(message.len() as u32).to_le_bytes()); // mov edx, len
    code.extend_from_slice(&[0xb8, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x05]); // mov eax,SYS_write; syscall
    code.extend_from_slice(&[
        0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xff, 0x0f, 0x05, 0x0f, 0x0b,
    ]); // mov eax,SYS_exit_group; xor edi,edi; syscall; ud2
    let message_offset = code.len();
    code.extend_from_slice(message);
    let message_vaddr = LOAD_ADDRESS + message_offset as u64;
    code[movabs_operand..movabs_operand + 8].copy_from_slice(&message_vaddr.to_le_bytes());

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf_with_args(&static_elf(&code), &["prog"], &[])
        .unwrap();

    let (log, exit_code, stdout, stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();

    assert_eq!(exit_code, 0);
    assert_eq!(stdout, b"hi\n");
    assert!(stderr.is_empty());
    assert_eq!(
        log.syscalls(),
        vec![
            "getpid".to_string(),
            "write".to_string(),
            "exit_group".to_string(),
        ],
    );
}

#[test]
fn tool_rpc_response_reaches_intercepted_static_elf_syscall() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM RPC round-trip test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    // Each getpid is intercepted instead of injected. The local tool advances
    // its ThreadState, sends the ordinal to GlobalState, and returns the typed
    // RPC response as the guest-visible syscall result. The exit hook then
    // sends a third typed RPC through the lifecycle GlobalRPC handle. Exit 1
    // if either guest-visible round trip produced an unexpected value.
    let code = [
        0x45, 0x31, 0xe4, // xor r12d, r12d
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, SYS_getpid
        0x0f, 0x05, // syscall
        0x3d, 0xe9, 0x03, 0x00, 0x00, // cmp eax, 1001
        0x41, 0x0f, 0x95, 0xc4, // setne r12b
        0xb8, 0x27, 0x00, 0x00, 0x00, // mov eax, SYS_getpid
        0x0f, 0x05, // syscall
        0x3d, 0xea, 0x03, 0x00, 0x00, // cmp eax, 1002
        0x0f, 0x95, 0xc0, // setne al
        0x0f, 0xb6, 0xc0, // movzx eax, al
        0x41, 0x09, 0xc4, // or r12d, eax
        0x44, 0x89, 0xe7, // mov edi, r12d
        0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, SYS_exit_group
        0x0f, 0x05, // syscall
        0x0f, 0x0b, // ud2
    ];
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&static_elf(&code), "/bin/rpc-round-trip")
        .unwrap();

    let (log, exit_code, stdout, stderr) = futures::executor::block_on(
        backend.run_static_elf_with_tool::<RpcRoundTripTool>(1000, true),
    )
    .unwrap();

    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        log.requests(),
        vec![
            (Pid::from_raw(1), 1),
            (Pid::from_raw(1), 2),
            (Pid::from_raw(1), 3),
        ]
    );
}

#[test]
fn counter_tools_aggregate_intercepted_static_elf_syscalls() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM counter-ELF test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let code = [
        0xb8, 0x27, 0x00, 0x00, 0x00, 0x0f, 0x05, // getpid
        0xb8, 0x27, 0x00, 0x00, 0x00, 0x0f, 0x05, // getpid
        0xb8, 0xe7, 0x00, 0x00, 0x00, // exit_group
        0x31, 0xff, 0x0f, 0x05, // status 0; syscall
        0x0f, 0x0b, // ud2
    ];
    let image = static_elf(&code);

    let mut direct_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    direct_backend
        .install_static_elf(&image, "/bin/counter-rpc")
        .unwrap();
    let (counter, exit_code, stdout, stderr) = futures::executor::block_on(
        direct_backend.run_static_elf_with_tool::<CounterTool>((), true),
    )
    .unwrap();
    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(counter.total(), 4);

    let mut hierarchical_backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    hierarchical_backend
        .install_static_elf(&image, "/bin/hierarchical-counter-rpc")
        .unwrap();
    let (counter, exit_code, stdout, stderr) = futures::executor::block_on(
        hierarchical_backend.run_static_elf_with_tool::<HierarchicalCounterTool>((), true),
    )
    .unwrap();
    assert_eq!(exit_code, 0);
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(
        counter.totals(),
        HierarchicalTotals {
            total_syscalls: 4,
            exited_procs: 1,
            exited_threads: 1,
        }
    );
}

#[test]
fn real_make_runs_a_shell_recipe_through_clone3_vfork() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM make test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }
    let root = TestDirectory::new();
    std::fs::write(
        root.0.join("Makefile"),
        "all: result.txt\nresult.txt:\n\tprintf 'make:42\\n' > result.txt\n",
    )
    .unwrap();
    run_host_program("/usr/bin/make", &["make", "-s"], &root.0);
    assert_eq!(
        std::fs::read(root.0.join("result.txt")).unwrap(),
        b"make:42\n"
    );
}

#[test]
fn real_gcc_compiles_an_object_through_child_processes() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM gcc test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }
    let root = TestDirectory::new();
    std::fs::write(
        root.0.join("fixture.c"),
        b"int hermit_compat(void) { return 42; }\n",
    )
    .unwrap();
    run_host_program(
        "/usr/bin/gcc",
        &[
            "gcc",
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-fno-ident",
            "-frandom-seed=hermit-gcc",
            "-c",
            "fixture.c",
            "-o",
            "fixture.o",
        ],
        &root.0,
    );
    assert!(root.0.join("fixture.o").is_file());
}

#[test]
fn real_patch_applies_exact_hunk_with_absent_xattrs() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM patch test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let root = TestDirectory::new();
    std::fs::write(root.0.join("file"), b"old\n").unwrap();
    std::fs::write(
        root.0.join("change.patch"),
        b"--- file\n+++ file\n@@ -1 +1 @@\n-old\n+new\n",
    )
    .unwrap();
    let (stdout, stderr) = run_host_program_captured(
        "/usr/bin/patch",
        &["patch", "--quiet", "--input=change.patch", "file"],
        &root.0,
    );
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
    assert_eq!(std::fs::read(root.0.join("file")).unwrap(), b"new\n");
}

#[test]
fn real_grep_uses_synthetic_process_maps_for_stack_discovery() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM grep test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let root = TestDirectory::new();
    std::fs::write(root.0.join("payload"), b"gamma\nbeta\nalpha\nbeta\n").unwrap();
    let (stdout, stderr) =
        run_host_program_captured("/usr/bin/grep", &["grep", "beta", "payload"], &root.0);
    assert_eq!(stdout, b"beta\nbeta\n");
    assert!(stderr.is_empty());
}

fn static_elf(code: &[u8]) -> Vec<u8> {
    static_elf_at(code, LOAD_ADDRESS)
}

fn static_elf_at(code: &[u8], load_address: u64) -> Vec<u8> {
    let mut image = vec![0; CODE_OFFSET + code.len()];

    image[..4].copy_from_slice(b"\x7fELF");
    image[4] = 2;
    image[5] = 1;
    image[6] = 1;
    put_u16(&mut image, 16, 2);
    put_u16(&mut image, 18, 62);
    put_u32(&mut image, 20, 1);
    put_u64(&mut image, 24, load_address);
    put_u64(&mut image, 32, 64);
    put_u16(&mut image, 52, 64);
    put_u16(&mut image, 54, 56);
    put_u16(&mut image, 56, 1);

    put_u32(&mut image, 64, 1);
    put_u32(&mut image, 68, 5);
    put_u64(&mut image, 72, CODE_OFFSET as u64);
    put_u64(&mut image, 80, load_address);
    put_u64(&mut image, 88, load_address);
    put_u64(&mut image, 96, code.len() as u64);
    put_u64(&mut image, 104, 0x2000);
    put_u64(&mut image, 112, 0x1000);
    image[CODE_OFFSET..].copy_from_slice(code);
    image
}

fn put_u16(image: &mut [u8], offset: usize, value: u16) {
    image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn native_and_kvm_prctl_names_keep_worker_local_and_format_procfs_leader_bytes() {
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "pthread-prctl-name",
        r#"
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <sys/prctl.h>
#include <unistd.h>

static const unsigned char leader_name[5] = {0xff, '\n', '\\', 'L', 0};
static const unsigned char stat_name[6] = {'(', 0xff, '\n', '\\', 'L', ')'};
static const unsigned char status_name[13] = {
    'N', 'a', 'm', 'e', ':', '\t', 0xff, '\\', 'n', '\\', '\\', 'L', '\n'
};

static const unsigned char *find_bytes(const unsigned char *haystack, size_t haystack_len,
                                       const unsigned char *needle, size_t needle_len) {
  if (needle_len > haystack_len) return 0;
  for (size_t i = 0; i <= haystack_len - needle_len; ++i) {
    if (memcmp(haystack + i, needle, needle_len) == 0) return haystack + i;
  }
  return 0;
}

static int read_proc(const char *path, unsigned char *buffer, size_t capacity) {
  int fd = open(path, O_RDONLY);
  if (fd < 0) return -1;
  ssize_t count = read(fd, buffer, capacity);
  close(fd);
  return count < 0 ? -1 : (int)count;
}

static void *worker(void *unused) {
  (void)unused;
  unsigned char worker_name[16] = "worker";
  unsigned char observed[16] = {0};
  unsigned char buffer[1024];
  if (prctl(PR_SET_NAME, worker_name) != 0) return (void *)(uintptr_t)1;
  if (prctl(PR_GET_NAME, observed) != 0) return (void *)(uintptr_t)2;
  if (memcmp(observed, worker_name, sizeof(observed)) != 0)
    return (void *)(uintptr_t)3;
  if (write(1, observed, sizeof(observed)) != sizeof(observed))
    return (void *)(uintptr_t)4;

  int count = read_proc("/proc/self/stat", buffer, sizeof(buffer));
  const unsigned char *found = count < 0 ? 0 : find_bytes(
      buffer, (size_t)count, stat_name, sizeof(stat_name));
  if (!found) return (void *)(uintptr_t)5;
  if (write(1, found, sizeof(stat_name)) != sizeof(stat_name))
    return (void *)(uintptr_t)6;

  count = read_proc("/proc/self/status", buffer, sizeof(buffer));
  found = count < 0 ? 0 : find_bytes(
      buffer, (size_t)count, status_name, sizeof(status_name));
  if (!found) return (void *)(uintptr_t)7;
  if (write(1, found, sizeof(status_name)) != sizeof(status_name))
    return (void *)(uintptr_t)8;
  return 0;
}

int main(void) {
  if (prctl(PR_SET_NAME, leader_name) != 0) return 10;
  pthread_t thread;
  if (pthread_create(&thread, 0, worker, 0) != 0) return 11;
  void *result = 0;
  if (pthread_join(thread, &result) != 0) return 12;
  if (result != 0) return 20 + (int)(uintptr_t)result;

  unsigned char observed[16] = {0};
  if (prctl(PR_GET_NAME, observed) != 0) return 13;
  if (memcmp(observed, leader_name, sizeof(leader_name)) != 0) return 14;
  if (write(1, observed, sizeof(observed)) != sizeof(observed)) return 15;
  return 0;
}
"#,
    );
    let mut expected = b"worker".to_vec();
    expected.resize(16, 0);
    expected.extend_from_slice(&[b'(', 0xff, b'\n', b'\\', b'L', b')']);
    expected.extend_from_slice(b"Name:\t\xff\\n\\\\L\n");
    expected.extend_from_slice(&[0xff, b'\n', b'\\', b'L']);
    expected.resize(51, 0);

    let native = std::process::Command::new(&executable)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    assert!(
        native.status.success(),
        "native task-name fixture failed: {native:?}"
    );
    assert_eq!(native.stdout, expected, "native Linux format changed");
    assert!(native.stderr.is_empty());

    if !kvm_available("native_and_kvm_prctl_names_keep_worker_local_and_format_procfs_leader_bytes")
    {
        return;
    }

    let executable = executable.to_str().unwrap();
    let (kvm_stdout, kvm_stderr) =
        run_host_program_captured(executable, &[executable], &directory.0);
    assert_eq!(kvm_stdout, native.stdout, "KVM must match native Linux");
    assert!(kvm_stderr.is_empty());
}

#[test]
fn kvm_direct_and_tool_match_prctl_identity_cell() {
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "prctl-identity",
        r#"
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>

int main(void) {
  const char *wanted = "hermit-probe";
  char name[16] = {0};
  int pdeath = -1;

  if (prctl(PR_SET_NAME, wanted, 0, 0, 0) != 0 ||
      prctl(PR_GET_NAME, name, 0, 0, 0) != 0 || strcmp(name, wanted) != 0)
    return 1;
  if (prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0)
    return 2;
  int dumpable_after_clear = prctl(PR_GET_DUMPABLE, 0, 0, 0, 0);
  if (prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) != 0)
    return 3;
  int dumpable_after_set = prctl(PR_GET_DUMPABLE, 0, 0, 0, 0);
  if (prctl(PR_SET_KEEPCAPS, 1, 0, 0, 0) != 0)
    return 4;
  int keepcaps_after_set = prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0);
  if (prctl(PR_SET_KEEPCAPS, 0, 0, 0, 0) != 0)
    return 5;
  int keepcaps_after_clear = prctl(PR_GET_KEEPCAPS, 0, 0, 0, 0);
  if (prctl(PR_GET_PDEATHSIG, &pdeath, 0, 0, 0) != 0)
    return 6;
  if (dumpable_after_clear != 0 || dumpable_after_set != 1 ||
      keepcaps_after_set != 1 || keepcaps_after_clear != 0 || pdeath != 0)
    return 7;

  printf("prctl-identity name=%s dumpable_after_clear=%d dumpable_after_set=%d "
         "keepcaps_after_set=%d keepcaps_after_clear=%d pdeathsig_initial=%d\n",
         name, dumpable_after_clear, dumpable_after_set, keepcaps_after_set,
         keepcaps_after_clear, pdeath);
  return 0;
}
"#,
    );
    let expected = concat!(
        "prctl-identity name=hermit-probe dumpable_after_clear=0 ",
        "dumpable_after_set=1 keepcaps_after_set=1 keepcaps_after_clear=0 ",
        "pdeathsig_initial=0\n",
    )
    .as_bytes();
    let native = std::process::Command::new(&executable)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    assert!(native.status.success(), "native fixture failed: {native:?}");
    assert_eq!(native.stdout, expected);
    assert!(native.stderr.is_empty());

    if !kvm_available("kvm_direct_and_tool_match_prctl_identity_cell") {
        return;
    }

    let executable = executable.to_str().unwrap();
    let (direct_stdout, direct_stderr) =
        run_host_program_captured(executable, &[executable], &directory.0);
    assert_eq!(direct_stdout, expected);
    assert!(direct_stderr.is_empty());
    let (tool_stdout, tool_stderr) =
        run_host_program_with_tool_captured(executable, &[executable], &directory.0);
    assert_eq!(tool_stdout, expected);
    assert!(tool_stderr.is_empty());
}

#[test]
fn kvm_direct_and_tool_match_thp_disable_cell() {
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "thp-disable",
        r#"
#include <stdio.h>
#include <sys/prctl.h>

#ifndef PR_SET_THP_DISABLE
#define PR_SET_THP_DISABLE 41
#endif
#ifndef PR_GET_THP_DISABLE
#define PR_GET_THP_DISABLE 42
#endif

int main(void) {
  int set_on = prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0) == 0;
  int get_after_set = prctl(PR_GET_THP_DISABLE, 0, 0, 0, 0);
  int set_off = prctl(PR_SET_THP_DISABLE, 0, 0, 0, 0) == 0;
  int get_after_clear = prctl(PR_GET_THP_DISABLE, 0, 0, 0, 0);
  int ok = set_on + (get_after_set == 1) + set_off + (get_after_clear == 0);
  printf("thp ok=%d set_on=%d get_after_set=%d set_off=%d get_after_clear=%d\n",
         ok, set_on, get_after_set, set_off, get_after_clear);
  return ok == 4 ? 0 : 1;
}
"#,
    );
    let expected = b"thp ok=4 set_on=1 get_after_set=1 set_off=1 get_after_clear=0\n";
    let native = std::process::Command::new(&executable)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    assert!(native.status.success(), "native fixture failed: {native:?}");
    assert_eq!(native.stdout, expected);
    assert!(native.stderr.is_empty());

    if !kvm_available("kvm_direct_and_tool_match_thp_disable_cell") {
        return;
    }

    let executable = executable.to_str().unwrap();
    let (direct_stdout, direct_stderr) =
        run_host_program_captured(executable, &[executable], &directory.0);
    assert_eq!(direct_stdout, expected);
    assert!(direct_stderr.is_empty());
    let (tool_stdout, tool_stderr) =
        run_host_program_with_tool_captured(executable, &[executable], &directory.0);
    assert_eq!(tool_stdout, expected);
    assert!(tool_stderr.is_empty());
}

/// A LIVE pthread worker exercises the `pid != tid` path end to end.
///
/// ⚠️ THIS IS THE CASE THE REJECTED IMPLEMENTATION COULD NOT SEE. Every earlier
/// signal test ran with `pid == tid`, so validating a thread-directed target
/// against `state.pid` looked correct. Under a real worker it is wrong in both
/// directions at once: the worker's own `tkill(gettid(), 0)` was refused with
/// ESRCH, while a LEADER-targeted request was accepted and then evaluated
/// against the worker's signal state.
#[test]
fn real_pthread_worker_signals_itself_by_tid_and_is_refused_for_the_leader() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM worker-signal test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "worker-thread-signal-identity",
        r#"
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sys/syscall.h>
#include <unistd.h>

static atomic_int result;

static void *worker(void *unused) {
  (void)unused;
  pid_t pid = getpid();
  pid_t tid = (pid_t)syscall(SYS_gettid);

  // The premise. If a worker's tid equalled its pid this test would prove
  // nothing, which is exactly how the rejected version passed review's tests.
  if (tid == pid) {
    atomic_store(&result, 20);
    return NULL;
  }
  // A worker signalling ITSELF by tid must succeed.
  if (syscall(SYS_tkill, tid, 0) != 0) {
    atomic_store(&result, 21);
    return NULL;
  }
  // ...and by (tgid, tid).
  if (syscall(SYS_tgkill, pid, tid, 0) != 0) {
    atomic_store(&result, 22);
    return NULL;
  }
  // ⚠️ THE REFUSAL. Naming the LEADER from the worker must fail visibly rather
  // than being applied to this thread. Any success here is the rejected bug.
  if (syscall(SYS_tgkill, pid, pid, 0) == 0) {
    atomic_store(&result, 23);
    return NULL;
  }
  // A non-positive thread id is EINVAL, not ESRCH.
  if (syscall(SYS_tkill, 0, 0) == 0 || errno != EINVAL) {
    atomic_store(&result, 24);
    return NULL;
  }
  atomic_store(&result, 1);
  return NULL;
}

int main(void) {
  atomic_store(&result, 0);
  pthread_t thread;
  if (pthread_create(&thread, NULL, worker, NULL) != 0) {
    return 10;
  }
  if (pthread_join(thread, NULL) != 0) {
    return 11;
  }
  int observed = atomic_load(&result);
  return observed == 1 ? 0 : observed;
}
"#,
    );
    let executable = executable.to_str().unwrap();
    let image = std::fs::read(executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (_trace, code, _stdout, _stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))
            .unwrap();
    assert_eq!(
        code, 0,
        "worker signal-identity guest failed with code {code}; \
         20=pid==tid so the case is vacuous, 21=tkill(gettid()) refused, \
         22=tgkill(getpid(),gettid()) refused, 23=leader-targeted call WRONGLY \
         SUCCEEDED, 24=tkill(0) not EINVAL"
    );
}

#[derive(Default)]
struct ChildWaitEventLog {
    events: Mutex<Vec<BackendChildWaitEvent>>,
}

#[reverie::global_tool]
impl GlobalTool for ChildWaitEventLog {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, (): ()) {}

    async fn on_backend_child_wait_event(
        &self,
        event: BackendChildWaitEvent,
    ) -> Result<(), reverie::Error> {
        self.events
            .lock()
            .expect("child wait-event log poisoned")
            .push(event);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ChildWaitEventTool;

#[reverie::tool]
impl Tool for ChildWaitEventTool {
    type GlobalState = ChildWaitEventLog;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::fork, Sysno::wait4, Sysno::rt_sigaction]);
        subscriptions
    }
}

#[test]
fn child_waitability_callback_and_auto_reap_are_observed_on_real_kvm() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM child-lifecycle test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "child-waitability",
        r#"
#include <errno.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

static void child_exit(int code) {
  pid_t child = fork();
  if (child < 0) _exit(90);
  if (child == 0) _exit(code);
}

int main(void) {
  struct sigaction action = {0};
  action.sa_handler = (void (*)(int))0x4321;
  sigemptyset(&action.sa_mask);
  errno = 0;
  /* Installing a real SIGCHLD handler must SUCCEED, as it does natively and under
     ptrace. What this test is really for is unchanged below: with a real handler
     installed SIGCHLD does not auto-reap, so the child must still be waitable.
     (This handler address is never invoked.) */
  if (sigaction(SIGCHLD, &action, 0) != 0) return 10;

  child_exit(7);
  int status = 0;
  if (waitpid(-1, &status, 0) <= 0 || !WIFEXITED(status) ||
      WEXITSTATUS(status) != 7) return 11;

  action.sa_handler = SIG_IGN;
  action.sa_flags = 0;
  if (sigaction(SIGCHLD, &action, 0) != 0) return 12;
  child_exit(8);
  if (waitpid(-1, &status, 0) != -1 || errno != ECHILD) return 13;

  action.sa_handler = SIG_DFL;
  action.sa_flags = SA_NOCLDWAIT;
  if (sigaction(SIGCHLD, &action, 0) != 0) return 14;
  child_exit(9);
  if (waitpid(-1, &status, 0) != -1 || errno != ECHILD) return 15;

  return 0;
}
"#,
    );
    let executable = executable.to_str().unwrap();
    let image = std::fs::read(executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();

    let (global, code, _stdout, stderr) = futures::executor::block_on(
        backend.run_static_elf_with_tool::<ChildWaitEventTool>((), true),
    )
    .unwrap();
    assert_eq!(
        code,
        0,
        "child lifecycle guest failed with code {code}; stderr={}",
        String::from_utf8_lossy(&stderr),
    );

    let events = global
        .events
        .lock()
        .expect("child wait-event log poisoned")
        .clone();
    assert_eq!(
        events,
        vec![
            BackendChildWaitEvent {
                parent: Pid::from_raw(1),
                child: Pid::from_raw(2),
                state: BackendChildWaitState::Exited {
                    status: ExitStatus::Exited(7),
                    waitable: true
                },
            },
            BackendChildWaitEvent {
                parent: Pid::from_raw(1),
                child: Pid::from_raw(3),
                state: BackendChildWaitState::Exited {
                    status: ExitStatus::Exited(8),
                    waitable: false
                },
            },
            BackendChildWaitEvent {
                parent: Pid::from_raw(1),
                child: Pid::from_raw(4),
                state: BackendChildWaitState::Exited {
                    status: ExitStatus::Exited(9),
                    waitable: false
                },
            },
        ],
        "the backend callback must describe every real terminal waitability transition",
    );
}

const PRCTL_REVIEW_REGRESSION: &str = r###"#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define REQUIRE(expression) do { if (!(expression)) { fprintf(stderr, "line=%d errno=%d: %s\n", __LINE__, errno, #expression); return 91; } } while (0)

static long call(unsigned long option, unsigned long second, unsigned long third,
                 unsigned long fourth, unsigned long fifth, unsigned long sixth) {
    errno = 0;
    return syscall(SYS_prctl, option, second, third, fourth, fifth, sixth);
}

static int check_name(const unsigned char expected[16]) {
    unsigned char actual[48], wanted[48];
    memset(actual, 0xa5, sizeof(actual));
    memset(wanted, 0xa5, sizeof(wanted));
    memcpy(wanted + 16, expected, 16);
    REQUIRE(call(PR_GET_NAME, (uintptr_t)(actual + 16), 17, 18, 19, UINT64_MAX) == 0);
    REQUIRE(memcmp(actual, wanted, sizeof(actual)) == 0);
    return 0;
}

static int names(int inspect_comm) {
    unsigned char *pages = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(pages != MAP_FAILED);
    memset(pages, 'x', 8192);
    REQUIRE(mprotect(pages + 4096, 4096, PROT_NONE) == 0);
    memcpy(pages + 4096 - 15, "ABCDEFGHIJKLMNO", 15);
    REQUIRE(call(PR_SET_NAME, (uintptr_t)(pages + 4096 - 15), 17, 18, 19, UINT64_MAX) == 0);
    REQUIRE(check_name((unsigned char[16]){"ABCDEFGHIJKLMNO"}) == 0);
    REQUIRE(call(PR_SET_NAME, (uintptr_t)(pages + 4096 - 14), 0, 0, 0, 0) == -1 && errno == EFAULT);
    REQUIRE(check_name((unsigned char[16]){"ABCDEFGHIJKLMNO"}) == 0);
    REQUIRE(call(PR_SET_NAME, (uintptr_t)(pages + 4096), 0, 0, 0, 0) == -1 && errno == EFAULT);
    REQUIRE(check_name((unsigned char[16]){"ABCDEFGHIJKLMNO"}) == 0);
    REQUIRE(call(PR_SET_NAME, UINT64_MAX, 0, 0, 0, 0) == -1 && errno == EFAULT);
    REQUIRE(check_name((unsigned char[16]){"ABCDEFGHIJKLMNO"}) == 0);
    pages[4095] = 0;
    REQUIRE(call(PR_SET_NAME, (uintptr_t)(pages + 4095), 0, 0, 0, 0) == 0);
    REQUIRE(check_name((unsigned char[16]){0}) == 0);
    const unsigned char special[16] = {0xff, '\n', '\\', '\t', '\r', ')', 0};
    REQUIRE(call(PR_SET_NAME, (uintptr_t)special, 0, 0, 0, 0) == 0);
    REQUIRE(check_name(special) == 0);
    unsigned char status[8192];
    int descriptor = open("/proc/self/status", O_RDONLY);
    REQUIRE(descriptor >= 0);
    ssize_t count = read(descriptor, status, sizeof(status));
    const unsigned char name_line[] = "Name:\t\xff\\n\\\\\t\r)\n";
    REQUIRE(count >= (ssize_t)(sizeof(name_line)-1));
    REQUIRE(memcmp(status, name_line, sizeof(name_line)-1) == 0);
    REQUIRE(close(descriptor) == 0);
    if (!inspect_comm) {
        REQUIRE(munmap(pages, 8192) == 0);
        puts("name truncation/fault atomicity/ignored args/proc escaping: PASS");
        return 0;
    }
    descriptor = open("/proc/self/comm", O_RDONLY);
    REQUIRE(descriptor >= 0);
    unsigned char comm[32], wanted[32];
    memset(comm, 0xa5, sizeof(comm));
    memset(wanted, 0xa5, sizeof(wanted));
    memcpy(wanted, special, 6);
    wanted[6] = '\n';
    REQUIRE(read(descriptor, comm, sizeof(comm)) == 7);
    REQUIRE(memcmp(comm, wanted, sizeof(comm)) == 0);
    REQUIRE(close(descriptor) == 0);
    REQUIRE(mprotect(pages + 4096, 4096, PROT_READ | PROT_WRITE) == 0);
    REQUIRE(munmap(pages, 8192) == 0);
    puts("name truncation/fault atomicity/ignored args/proc escaping: PASS");
    return 0;
}

static int copyout(int selection) {
    unsigned char *pages = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(pages != MAP_FAILED);
    unsigned char wanted[8192];
    memset(pages, 0xa5, 8192);
    memset(wanted, 0xa5, sizeof(wanted));
    REQUIRE(call(PR_SET_NAME, (uintptr_t)"ABCDEFGHIJKLMNO", 0, 0, 0, 0) == 0);
    REQUIRE(call(PR_SET_PDEATHSIG, 0, 0, 0, 0, 0) == 0);
    REQUIRE(mprotect(pages, 4096, PROT_READ) == 0);
    for (unsigned option = 0; option < 2; ++option) {
        if (selection >= 0 && option != (unsigned)selection) continue;
        long result = call(option ? PR_GET_PDEATHSIG : PR_GET_NAME, (uintptr_t)(pages+100), 0, 0, 0, 0);
        int saved_errno = errno;
        unsigned changed = 0;
        for (unsigned index = 0; index < sizeof(wanted); ++index) changed += pages[index] != wanted[index];
        printf("copyout option=%u result=%ld errno=%d changed_bytes=%u\n", option ? PR_GET_PDEATHSIG : PR_GET_NAME, result, saved_errno, changed);
        REQUIRE(result == -1 && saved_errno == EFAULT);
        REQUIRE(memcmp(pages, wanted, sizeof(wanted)) == 0);
    }
    REQUIRE(mprotect(pages, 4096, PROT_READ | PROT_WRITE) == 0);
    if (selection == 0 || selection == 1) {
        REQUIRE(munmap(pages, 8192) == 0);
        puts("read-only copyout exact EFAULT/full8192 unchanged: PASS");
        return 0;
    }
    REQUIRE(mprotect(pages + 4096, 4096, PROT_NONE) == 0);
    long result = call(PR_GET_NAME, (uintptr_t)(pages + 4096 - 8), 0, 0, 0, 0);
    int saved = errno;
    REQUIRE(result == -1 && saved == EFAULT);
    REQUIRE(mprotect(pages + 4096, 4096, PROT_READ | PROT_WRITE) == 0);
    printf("GET_NAME cross-page result=%ld errno=%d changed:", result, saved);
    for (unsigned index = 0; index < 8192; ++index) if (pages[index] != wanted[index]) printf(" %u=%02x", index, pages[index]);
    puts("");
    memcpy(wanted + 4088, "ABCDEFGH", 8);
    REQUIRE(memcmp(pages, wanted, sizeof(wanted)) == 0);
    REQUIRE(munmap(pages, 8192) == 0);
    puts("cross-page name copyout exact prefix/full8192 oracle: PASS");
    return 0;
}

static int thp(void) {
    const unsigned long flags[] = {0, 1, 2, 3, 4, 1UL << 32, UINT64_MAX};
    for (unsigned disable = 0; disable < 2; ++disable) {
        for (unsigned index = 0; index < sizeof(flags)/sizeof(flags[0]); ++index) {
            REQUIRE(call(PR_SET_THP_DISABLE, 0, 0, 0, 0, 0) == 0);
            long result = call(PR_SET_THP_DISABLE, disable, flags[index], 0, 0, UINT64_MAX);
            int saved = errno;
            long state = call(PR_GET_THP_DISABLE, 0, 0, 0, 0, UINT64_MAX);
            printf("THP disable=%u flags=%lu result=%ld errno=%d state=%ld\n", disable, flags[index], result, saved, state);
        }
    }
    REQUIRE(call(PR_SET_THP_DISABLE, 2, 0, 0, 0, UINT64_MAX) == 0);
    REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, UINT64_MAX) == 1);
    for (unsigned argument = 1; argument < 5; ++argument) {
        unsigned long args[5] = {PR_GET_THP_DISABLE, 0, 0, 0, 0};
        args[argument] = 1;
        REQUIRE(call(args[0], args[1], args[2], args[3], args[4], UINT64_MAX) == -1 && errno == EINVAL);
        REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, 0) == 1);
    }
    REQUIRE(call(PR_SET_THP_DISABLE, 0, 0, 0, 0, 0) == 0);
    puts("THP ignored sixth/strict GET arguments/nonzero disable: PASS");
    return 0;
}

static int modern_thp(void) {
    REQUIRE(call(PR_SET_THP_DISABLE, 0, 0, 0, 0, 0) == 0);
    REQUIRE(call(PR_SET_THP_DISABLE, 1, 2, 0, 0, UINT64_MAX) == 0);
    REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, UINT64_MAX) == 3);
    REQUIRE(call(PR_SET_THP_DISABLE, 1, 1, 0, 0, 0) == -1 && errno == EINVAL);
    REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, 0) == 3);
    REQUIRE(call(PR_SET_THP_DISABLE, 0, 0, 0, 0, 0) == 0);
    REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, 0) == 0);
    puts("THP EXCEPT_ADVISED flag2 state3 and invalidflag atomicity: PASS");
    return 0;
}

static void *worker(void *unused) {
    (void)unused;
    if (check_name((unsigned char[16]){"leader"}) != 0) return (void *)1;
    if (call(PR_SET_NAME, (uintptr_t)"worker", 0, 0, 0, 0) != 0) return (void *)2;
    if (check_name((unsigned char[16]){"worker"}) != 0) return (void *)3;
    if (call(PR_SET_THP_DISABLE, 0, 0, 0, 0, 0) != 0) return (void *)4;
    return NULL;
}

static int shared_process(void *unused) {
    (void)unused;
    if (check_name((unsigned char[16]){"leader"}) != 0) return 1;
    if (call(PR_SET_NAME, (uintptr_t)"vm-child", 0, 0, 0, 0) != 0) return 2;
    if (call(PR_SET_THP_DISABLE, 0, 0, 0, 0, 0) != 0) return 3;
    return 0;
}

static int lifecycle(const char *executable) {
    REQUIRE(call(PR_SET_NAME, (uintptr_t)"leader", 0, 0, 0, 0) == 0);
    REQUIRE(call(PR_SET_THP_DISABLE, 1, 0, 0, 0, 0) == 0);
    pthread_t thread;
    REQUIRE(pthread_create(&thread, NULL, worker, NULL) == 0);
    void *thread_result;
    REQUIRE(pthread_join(thread, &thread_result) == 0 && thread_result == NULL);
    REQUIRE(check_name((unsigned char[16]){"leader"}) == 0);
    REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, 0) == 0);
    REQUIRE(call(PR_SET_THP_DISABLE, 1, 0, 0, 0, 0) == 0);
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) _exit(shared_process(NULL));
    int status;
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, 0) == 1);
    REQUIRE(check_name((unsigned char[16]){"leader"}) == 0);
    void *stack = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(stack != MAP_FAILED);
    child = clone(shared_process, (char *)stack + 65536, CLONE_VM | CLONE_VFORK | SIGCHLD, NULL);
    REQUIRE(child >= 0);
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, 0) == 0);
    REQUIRE(check_name((unsigned char[16]){"leader"}) == 0);
    REQUIRE(munmap(stack, 65536) == 0);
    REQUIRE(call(PR_SET_THP_DISABLE, 1, 0, 0, 0, 0) == 0);
    child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        execl(executable, "fake-argv-zero", "after-exec", NULL);
        _exit(92);
    }
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(check_name((unsigned char[16]){"leader"}) == 0);
    puts("thread-local name/mm THP sharing/fork isolation/exec inheritance: PASS");
    return 0;
}

int main(int argc, char **argv) {
    REQUIRE(argc == 2);
    if (strcmp(argv[1], "names") == 0) return names(1);
    if (strcmp(argv[1], "names-core") == 0) return names(0);
    if (strcmp(argv[1], "copyout") == 0) return copyout(-1);
    if (strcmp(argv[1], "copyout-name") == 0) return copyout(0);
    if (strcmp(argv[1], "copyout-pdeath") == 0) return copyout(1);
    if (strcmp(argv[1], "copyout-partial") == 0) return copyout(2);
    if (strcmp(argv[1], "thp") == 0) return thp();
    if (strcmp(argv[1], "modern-thp") == 0) return modern_thp();
    if (strcmp(argv[1], "lifecycle") == 0) return lifecycle(argv[0]);
    if (strcmp(argv[1], "after-exec") == 0) {
        REQUIRE(check_name((unsigned char[16]){"native-prctl"}) == 0);
        REQUIRE(call(PR_GET_THP_DISABLE, 0, 0, 0, 0, 0) == 1);
        return 0;
    }
    return 93;
}
"###;

fn review_537_case(mode: &str) {
    assert!(kvm_available("review_537_case"));
    let directory = TestDirectory::new();
    let executable = compile_c_program(&directory.0, "native-prctl", PRCTL_REVIEW_REGRESSION);
    let native = std::process::Command::new(&executable)
        .arg(mode)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    println!(
        "NATIVE mode={mode} status={:?} stdout={} stderr={}",
        native.status.code(),
        String::from_utf8_lossy(&native.stdout),
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(native.status.code(), Some(0));
    assert!(native.stderr.is_empty());
    let image = std::fs::read(&executable).unwrap();
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_with_context(
            &image,
            &[executable.to_str().unwrap(), mode],
            &["PATH=/usr/bin:/bin"],
            &directory.0,
        )
        .unwrap();
    let (code, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    println!(
        "KVM mode={mode} status={code} stdout={} stderr={}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, native.stdout);
    assert!(stderr.is_empty());
}

#[test]
fn review_537_name_core() {
    review_537_case("names-core");
}
#[test]
fn review_537_get_name_readonly() {
    review_537_case("copyout-name");
}
#[test]
fn review_537_get_pdeath_readonly() {
    review_537_case("copyout-pdeath");
}
#[test]
fn review_537_get_name_partial() {
    review_537_case("copyout-partial");
}
#[test]
fn review_537_modern_thp() {
    review_537_case("modern-thp");
}
#[test]
fn review_537_thread_mm_lifecycle() {
    review_537_case("lifecycle");
}

const PRCTL_EXPANDED_REGRESSION: &str = r###"#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define REQUIRE(condition) do { if (!(condition)) { fprintf(stderr, "line %d errno %d: %s\n", __LINE__, errno, #condition); return 91; } } while (0)

static long control(unsigned long option, unsigned long second, unsigned long third, unsigned long fourth, unsigned long fifth) {
    errno = 0;
    return syscall(SYS_prctl, option, second, third, fourth, fifth, UINT64_MAX);
}

static int boundaries(void) {
    unsigned char *pages = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(pages != MAP_FAILED);
    REQUIRE(prctl(PR_SET_NAME, "ABCDEFGHIJKLMNO", 0, 0, 0) == 0);
    REQUIRE(prctl(PR_SET_PDEATHSIG, 0, 0, 0, 0) == 0);
    unsigned char wanted[8192];
    const int protections[] = {PROT_READ, PROT_NONE};
    const int options[] = {PR_GET_PDEATHSIG, PR_GET_NAME};
    for (unsigned option = 0; option < 2; ++option) {
        for (unsigned protection = 0; protection < 2; ++protection) {
            for (unsigned offset = 4080; offset <= 4097; ++offset) {
                memset(pages, 0xa5, 8192);
                memset(wanted, 0xa5, sizeof(wanted));
                REQUIRE(mprotect(pages + 4096, 4096, protections[protection]) == 0);
                long result = control(options[option], (uintptr_t)(pages + offset), 0, 0, 0);
                int error = errno;
                REQUIRE(mprotect(pages + 4096, 4096, PROT_READ | PROT_WRITE) == 0);
                unsigned length = option ? 16 : 4;
                unsigned prefix = offset >= 4096 ? 0 : 4096 - offset;
                if (prefix > length) prefix = length;
                unsigned copied = option ? prefix : (prefix == length ? length : 0);
                if (option) memcpy(wanted + offset, "ABCDEFGHIJKLMNO", copied);
                else memset(wanted + offset, 0, copied);
                REQUIRE(result == (prefix == length ? 0 : -1));
                REQUIRE(error == (prefix == length ? 0 : EFAULT));
                REQUIRE(memcmp(pages, wanted, sizeof(wanted)) == 0);
            }
        }
    }
    REQUIRE(control(PR_GET_NAME, UINT64_MAX, 0, 0, 0) == -1 && errno == EFAULT);
    REQUIRE(control(PR_GET_PDEATHSIG, UINT64_MAX, 0, 0, 0) == -1 && errno == EFAULT);
    REQUIRE(munmap(pages, 8192) == 0);
    puts("72 scalar/name boundary full-buffer cases: PASS");
    return 0;
}

static int thp_modes(void) {
    const unsigned long disables[] = {0, 1, 2, UINT64_MAX};
    const unsigned long flags[] = {0, 1, 2, 3, 4, 1UL << 32, UINT64_MAX};
    for (unsigned disable = 0; disable < 4; ++disable) {
        for (unsigned flag = 0; flag < 7; ++flag) {
            REQUIRE(control(PR_SET_THP_DISABLE, 1, 2, 0, 0) == 0);
            int valid = flags[flag] == 0 || (disables[disable] != 0 && flags[flag] == 2);
            long result = control(PR_SET_THP_DISABLE, disables[disable], flags[flag], 0, 0);
            REQUIRE(result == (valid ? 0 : -1));
            REQUIRE(errno == (valid ? 0 : EINVAL));
            long wanted = valid ? (disables[disable] ? 1 | flags[flag] : 0) : 3;
            REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == wanted);
        }
    }
    for (unsigned argument = 1; argument < 5; ++argument) {
        unsigned long args[5] = {PR_GET_THP_DISABLE, 0, 0, 0, 0};
        REQUIRE(control(PR_SET_THP_DISABLE, 1, 2, 0, 0) == 0);
        args[argument] = UINT64_MAX;
        REQUIRE(control(args[0], args[1], args[2], args[3], args[4]) == -1 && errno == EINVAL);
        REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 3);
    }
    REQUIRE(control(PR_SET_THP_DISABLE, 0, 0, 1, 0) == -1 && errno == EINVAL);
    REQUIRE(control(PR_SET_THP_DISABLE, 0, 0, 0, 1) == -1 && errno == EINVAL);
    REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 3);
    puts("THP 0/1/3 full-width matrix and ignored sixth: PASS");
    return 0;
}

static int expect_output(unsigned char *pages, size_t length, int writable) {
    unsigned char *wanted = malloc(length);
    REQUIRE(wanted != NULL);
    memcpy(wanted, pages, length);
    long result = control(PR_GET_NAME, (uintptr_t)(pages + 128), 0, 0, 0);
    REQUIRE(result == (writable ? 0 : -1));
    REQUIRE(errno == (writable ? 0 : EFAULT));
    if (writable) memcpy(wanted + 128, "ABCDEFGHIJKLMNO", 16);
    REQUIRE(memcmp(pages, wanted, length) == 0);
    free(wanted);
    return 0;
}

static void *protect_worker(void *pages) {
    if (mprotect(pages, 4096, PROT_READ) != 0) return (void *)1;
    return NULL;
}

static int permissions(void) {
    REQUIRE(prctl(PR_SET_NAME, "ABCDEFGHIJKLMNO", 0, 0, 0) == 0);
    const int protections[] = {PROT_NONE, PROT_READ, PROT_WRITE, PROT_EXEC, PROT_READ | PROT_EXEC, PROT_READ | PROT_WRITE, PROT_READ | PROT_WRITE | PROT_EXEC};
    unsigned char *pages = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(pages != MAP_FAILED);
    unsigned char wanted[8192];
    for (unsigned index = 0; index < sizeof(protections) / sizeof(protections[0]); ++index) {
        memset(pages, 0xa5, 8192);
        memset(wanted, 0xa5, sizeof(wanted));
        REQUIRE(mprotect(pages, 4096, protections[index]) == 0);
        long result = control(PR_GET_NAME, (uintptr_t)(pages + 128), 0, 0, 0);
        int error = errno;
        int writable = (protections[index] & PROT_WRITE) != 0;
        REQUIRE(mprotect(pages, 4096, PROT_READ | PROT_WRITE) == 0);
        REQUIRE(result == (writable ? 0 : -1));
        REQUIRE(error == (writable ? 0 : EFAULT));
        if (writable) memcpy(wanted + 128, "ABCDEFGHIJKLMNO", 16);
        REQUIRE(memcmp(pages, wanted, sizeof(wanted)) == 0);
    }
    REQUIRE(mprotect(pages, 4096, PROT_READ) == 0);
    REQUIRE(expect_output(pages, 8192, 0) == 0);
    REQUIRE(mmap(pages, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == pages);
    REQUIRE(expect_output(pages, 8192, 1) == 0);
    REQUIRE(mmap(pages, 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == pages);
    REQUIRE(expect_output(pages, 8192, 0) == 0);
    REQUIRE(mprotect(pages, 4096, PROT_READ | PROT_WRITE) == 0);
    pthread_t thread;
    REQUIRE(pthread_create(&thread, NULL, protect_worker, pages) == 0);
    void *thread_result = (void *)1;
    REQUIRE(pthread_join(thread, &thread_result) == 0 && thread_result == NULL);
    REQUIRE(expect_output(pages, 8192, 0) == 0);
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        REQUIRE(expect_output(pages, 8192, 0) == 0);
        REQUIRE(mprotect(pages, 4096, PROT_READ | PROT_WRITE) == 0);
        REQUIRE(expect_output(pages, 8192, 1) == 0);
        _exit(0);
    }
    int status;
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(expect_output(pages, 8192, 0) == 0);
    REQUIRE(munmap(pages, 8192) == 0);
    REQUIRE(mmap(pages, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == pages);
    REQUIRE(expect_output(pages, 8192, 1) == 0);
    REQUIRE(munmap(pages, 8192) == 0);
    for (unsigned writable = 0; writable < 2; ++writable) {
        pages = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        REQUIRE(pages != MAP_FAILED);
        memset(pages, 0xa5, 4096);
        REQUIRE(mprotect(pages, 4096, PROT_READ | (writable ? PROT_WRITE : 0)) == 0);
        pages = mremap(pages, 4096, 8192, MREMAP_MAYMOVE);
        REQUIRE(pages != MAP_FAILED);
        REQUIRE(expect_output(pages, 8192, writable) == 0);
        REQUIRE(expect_output(pages + 4096, 4096, writable) == 0);
        pages = mremap(pages, 8192, 4096, MREMAP_MAYMOVE);
        REQUIRE(pages != MAP_FAILED);
        REQUIRE(expect_output(pages, 4096, writable) == 0);
        REQUIRE(munmap(pages, 4096) == 0);
    }
    puts("mapping/mprotect/replacement/thread/fork/remap copyout: PASS");
    return 0;
}

static void *mode_worker(void *unused) {
    (void)unused;
    if (control(PR_GET_THP_DISABLE, 0, 0, 0, 0) != 3) return (void *)1;
    if (control(PR_SET_THP_DISABLE, 1, 0, 0, 0) != 0) return (void *)2;
    return NULL;
}

static int mode_shared(void *unused) {
    (void)unused;
    if (control(PR_GET_THP_DISABLE, 0, 0, 0, 0) != 3) return 1;
    if (control(PR_SET_THP_DISABLE, 0, 0, 0, 0) != 0) return 2;
    return 0;
}

static int mode_lifetime(const char *executable) {
    REQUIRE(control(PR_SET_THP_DISABLE, 1, 2, 0, 0) == 0);
    pthread_t thread;
    REQUIRE(pthread_create(&thread, NULL, mode_worker, NULL) == 0);
    void *result = (void *)1;
    REQUIRE(pthread_join(thread, &result) == 0 && result == NULL);
    REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 1);
    REQUIRE(control(PR_SET_THP_DISABLE, 1, 2, 0, 0) == 0);
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 3);
        REQUIRE(control(PR_SET_THP_DISABLE, 0, 0, 0, 0) == 0);
        _exit(0);
    }
    int status;
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 3);
    unsigned char *stack = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(stack != MAP_FAILED);
    child = clone(mode_shared, stack + 65536, CLONE_VM | CLONE_VFORK | SIGCHLD, NULL);
    REQUIRE(child >= 0);
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 0);
    REQUIRE(munmap(stack, 65536) == 0);
    REQUIRE(control(PR_SET_THP_DISABLE, 1, 2, 0, 0) == 0);
    child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        execl(executable, "unrelated-argv-zero", "mode-after-exec", NULL);
        _exit(92);
    }
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 3);
    puts("THP mode3 thread/VM sharing, fork isolation, exec: PASS");
    return 0;
}

int main(int argc, char **argv) {
    REQUIRE(argc == 2);
    if (strcmp(argv[1], "boundaries") == 0) return boundaries();
    if (strcmp(argv[1], "thp-modes") == 0) return thp_modes();
    if (strcmp(argv[1], "permissions") == 0) return permissions();
    if (strcmp(argv[1], "mode-lifetime") == 0) return mode_lifetime(argv[0]);
    if (strcmp(argv[1], "mode-after-exec") == 0) {
        REQUIRE(control(PR_GET_THP_DISABLE, 0, 0, 0, 0) == 3);
        return 0;
    }
    return 93;
}
"###;

fn run_expanded_prctl(mode: &str) {
    assert!(kvm_available("run_expanded_prctl"));
    let directory = TestDirectory::new();
    let executable = compile_c_program(&directory.0, "expanded-prctl", PRCTL_EXPANDED_REGRESSION);
    let native = std::process::Command::new(&executable)
        .arg(mode)
        .current_dir(&directory.0)
        .output()
        .unwrap();
    assert_eq!(
        native.status.code(),
        Some(0),
        "native mode={mode}: {native:?}"
    );
    assert!(native.stderr.is_empty());
    let executable = executable.to_str().unwrap();
    let (stdout, stderr) = run_host_program_captured(executable, &[executable, mode], &directory.0);
    assert_eq!(stdout, native.stdout);
    assert!(stderr.is_empty());
    let (stdout, stderr) =
        run_host_program_with_tool_captured(executable, &[executable, mode], &directory.0);
    assert_eq!(stdout, native.stdout);
    assert!(stderr.is_empty());
}

#[test]
fn repair_prctl_scalar_and_name_boundaries() {
    run_expanded_prctl("boundaries");
}
#[test]
fn repair_prctl_full_thp_modes() {
    run_expanded_prctl("thp-modes");
}
#[test]
fn repair_prctl_permissions_and_lifetime() {
    run_expanded_prctl("permissions");
}
#[test]
fn repair_prctl_mode_three_lifetime() {
    run_expanded_prctl("mode-lifetime");
}

const PRCTL_DENY_KVM: &str = r###"#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <string.h>
#include <sys/types.h>

static int open_forward(const char *symbol, const char *path, int flags, va_list arguments) {
    if (strcmp(path, "/dev/kvm") == 0) {
        errno = EACCES;
        return -1;
    }
    int (*next)(const char *, int, ...) = dlsym(RTLD_NEXT, symbol);
    if (!next) {
        errno = ENOSYS;
        return -1;
    }
    if ((flags & O_CREAT) || (flags & O_TMPFILE) == O_TMPFILE) {
        mode_t mode = va_arg(arguments, unsigned int);
        return next(path, flags, mode);
    }
    return next(path, flags);
}

int open(const char *path, int flags, ...) {
    va_list arguments;
    va_start(arguments, flags);
    int result = open_forward("open", path, flags, arguments);
    va_end(arguments);
    return result;
}

int open64(const char *path, int flags, ...) {
    va_list arguments;
    va_start(arguments, flags);
    int result = open_forward("open64", path, flags, arguments);
    va_end(arguments);
    return result;
}
"###;

fn prctl_elf_copyout_image(kind: &str, writable: bool, name: bool, output_offset: u64) -> Vec<u8> {
    fn immediate(code: &mut Vec<u8>, register: u8, value: u64) {
        code.extend_from_slice(&[0x48, register]);
        code.extend_from_slice(&value.to_le_bytes());
    }
    fn write_page(code: &mut Vec<u8>, address: u64) {
        immediate(code, 0xb8, 1);
        immediate(code, 0xbf, 1);
        immediate(code, 0xbe, address);
        immediate(code, 0xba, 4096);
        code.extend_from_slice(&[0x0f, 0x05]);
    }
    fn write_result(code: &mut Vec<u8>) {
        code.push(0x50);
        immediate(code, 0xb8, 1);
        immediate(code, 0xbf, 1);
        code.extend_from_slice(&[0x48, 0x89, 0xe6]);
        immediate(code, 0xba, 8);
        code.extend_from_slice(&[0x0f, 0x05, 0x58]);
    }
    let address = LOAD_ADDRESS + 0x4000;
    let succeeds = writable || kind.starts_with("bss");
    let mut code = Vec::new();
    write_page(&mut code, address);
    if succeeds {
        immediate(&mut code, 0xbf, address);
        immediate(&mut code, 0xb9, 4096);
        immediate(&mut code, 0xb8, 0x5a);
        code.extend_from_slice(&[0xfc, 0xf3, 0xaa]);
    }
    write_page(&mut code, address);
    immediate(&mut code, 0xb8, libc::SYS_prctl as u64);
    immediate(
        &mut code,
        0xbf,
        if name {
            libc::PR_SET_NAME
        } else {
            libc::PR_SET_PDEATHSIG
        } as u64,
    );
    let name_operand = code.len() + 2;
    immediate(&mut code, 0xbe, 0);
    code.extend_from_slice(&[0x0f, 0x05]);
    write_result(&mut code);
    immediate(&mut code, 0xb8, libc::SYS_prctl as u64);
    immediate(
        &mut code,
        0xbf,
        if name {
            libc::PR_GET_NAME
        } else {
            libc::PR_GET_PDEATHSIG
        } as u64,
    );
    immediate(&mut code, 0xbe, address + output_offset);
    code.extend_from_slice(&[0x0f, 0x05]);
    write_result(&mut code);
    write_page(&mut code, address);
    immediate(&mut code, 0xb8, 60);
    immediate(&mut code, 0xbf, 0);
    code.extend_from_slice(&[0x0f, 0x05]);
    if name {
        let name_address = LOAD_ADDRESS + code.len() as u64;
        code[name_operand..name_operand + 8].copy_from_slice(&name_address.to_le_bytes());
        code.extend_from_slice(b"ABCDEFGHIJKLMNO\0");
    }
    let mut image = static_elf(&code);
    image.resize(0x4000, 0);
    image[0x2000..0x4000].fill(if kind.starts_with("bss") { 0 } else { 0x5a });
    if kind == "file-bss" {
        image[0x3800..0x4000].fill(0);
    }
    let overlap = kind != "single";
    put_u16(&mut image, 56, if overlap { 3 } else { 2 });
    for index in 1..=if overlap { 2 } else { 1 } {
        let second = index == 2;
        let split = second && (kind == "file-split" || kind == "bss-split");
        let bss = second && kind.starts_with("bss");
        let file_tail = second && kind == "file-bss";
        let flags = if second || !overlap {
            writable
        } else {
            !writable
        };
        let offset = if second { 0x3000 } else { 0x2000 } + if split { 0x800 } else { 0 };
        let size = if split { 0x800 } else { 0x1000 };
        let header = 64 + index * 56;
        put_u32(&mut image, header, 1);
        put_u32(&mut image, header + 4, if flags { 6 } else { 4 });
        for (field, value) in [
            (8, offset),
            (16, address + if split { 0x800 } else { 0 }),
            (24, address + if split { 0x800 } else { 0 }),
            (
                32,
                if bss {
                    0
                } else if file_tail {
                    0x800
                } else {
                    size
                },
            ),
            (40, size),
            (48, 0x1000),
        ] {
            put_u64(&mut image, header + field, value);
        }
    }
    image
}

fn check_prctl_elf_copyout(kind: &str, writable: bool, name: bool, output_offset: u64) {
    assert!(kvm_available("check_prctl_elf_copyout"));
    let image = prctl_elf_copyout_image(kind, writable, name, output_offset);
    let executable = TestExecutable::new(&image);
    std::fs::set_permissions(&executable.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let native = std::process::Command::new(&executable.0).output().unwrap();
    assert_eq!(
        native.status.code(),
        Some(0),
        "native {kind}/{writable}/{name}/{output_offset}: {native:?}"
    );
    assert!(native.stderr.is_empty());
    let mut initial = vec![if kind.starts_with("bss") { 0 } else { 0x5a }; 4096];
    if kind == "file-bss" {
        initial[2048..].fill(0);
    }
    let succeeds = writable || kind.starts_with("bss");
    let before = if succeeds {
        vec![0x5a; 4096]
    } else {
        initial.clone()
    };
    let mut after = before.clone();
    let offset = usize::try_from(output_offset).unwrap();
    if succeeds {
        if name {
            after[offset..offset + 16].copy_from_slice(b"ABCDEFGHIJKLMNO\0");
        } else {
            after[offset..offset + 4].fill(0);
        }
        assert_ne!(after, before);
    }
    let mut expected = initial;
    expected.extend_from_slice(&before);
    expected.extend_from_slice(&0_i64.to_le_bytes());
    expected.extend_from_slice(
        &(if succeeds {
            0_i64
        } else {
            -i64::from(libc::EFAULT)
        })
        .to_le_bytes(),
    );
    expected.extend_from_slice(&after);
    assert_eq!(expected.len(), 3 * 4096 + 16);
    assert_eq!(
        native.stdout, expected,
        "fixed native oracle {kind}/{writable}/{name}/{output_offset}"
    );
    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .install_static_elf(&image, "prctl-elf-copyout")
        .unwrap();
    let (status, stdout, stderr) = backend.run_static_elf_captured().unwrap();
    assert_eq!(status, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout.len(), expected.len());
    assert!(
        stdout == expected,
        "{kind}/{writable}/{name}/{output_offset} first differing byte {:?}",
        stdout
            .iter()
            .zip(&expected)
            .position(|(actual, expected)| actual != expected)
    );
}

macro_rules! prctl_elf_copyout_case {
    ($test:ident, $kind:literal, $writable:literal, $name:literal, $offset:literal) => {
        #[test]
        fn $test() {
            check_prctl_elf_copyout($kind, $writable, $name, $offset);
        }
    };
}

prctl_elf_copyout_case!(
    prctl_elf_copyout_single_r_pdeath_lower,
    "single",
    false,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_single_r_pdeath_upper,
    "single",
    false,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_single_r_name_lower,
    "single",
    false,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_single_r_name_upper,
    "single",
    false,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_single_rw_pdeath_lower,
    "single",
    true,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_single_rw_pdeath_upper,
    "single",
    true,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_single_rw_name_lower,
    "single",
    true,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_single_rw_name_upper,
    "single",
    true,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_r_pdeath_lower,
    "file-full",
    false,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_r_pdeath_upper,
    "file-full",
    false,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_r_name_lower,
    "file-full",
    false,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_r_name_upper,
    "file-full",
    false,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_rw_pdeath_lower,
    "file-full",
    true,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_rw_pdeath_upper,
    "file-full",
    true,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_rw_name_lower,
    "file-full",
    true,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_full_rw_name_upper,
    "file-full",
    true,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_r_pdeath_lower,
    "file-split",
    false,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_r_pdeath_upper,
    "file-split",
    false,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_r_name_lower,
    "file-split",
    false,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_r_name_upper,
    "file-split",
    false,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_rw_pdeath_lower,
    "file-split",
    true,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_rw_pdeath_upper,
    "file-split",
    true,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_rw_name_lower,
    "file-split",
    true,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_split_rw_name_upper,
    "file-split",
    true,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_r_pdeath_lower,
    "bss-full",
    false,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_r_pdeath_upper,
    "bss-full",
    false,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_r_name_lower,
    "bss-full",
    false,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_r_name_upper,
    "bss-full",
    false,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_rw_pdeath_lower,
    "bss-full",
    true,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_rw_pdeath_upper,
    "bss-full",
    true,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_rw_name_lower,
    "bss-full",
    true,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_full_rw_name_upper,
    "bss-full",
    true,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_r_pdeath_lower,
    "bss-split",
    false,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_r_pdeath_upper,
    "bss-split",
    false,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_r_name_lower,
    "bss-split",
    false,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_r_name_upper,
    "bss-split",
    false,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_rw_pdeath_lower,
    "bss-split",
    true,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_rw_pdeath_upper,
    "bss-split",
    true,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_rw_name_lower,
    "bss-split",
    true,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_bss_split_rw_name_upper,
    "bss-split",
    true,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_r_pdeath_lower,
    "file-bss",
    false,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_r_pdeath_upper,
    "file-bss",
    false,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_r_name_lower,
    "file-bss",
    false,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_r_name_upper,
    "file-bss",
    false,
    true,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_rw_pdeath_lower,
    "file-bss",
    true,
    false,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_rw_pdeath_upper,
    "file-bss",
    true,
    false,
    2304
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_rw_name_lower,
    "file-bss",
    true,
    true,
    128
);
prctl_elf_copyout_case!(
    prctl_elf_copyout_file_bss_rw_name_upper,
    "file-bss",
    true,
    true,
    2304
);

#[test]
fn repair_prctl_required_kvm_is_not_optional() {
    let directory = TestDirectory::new();
    let library = compile_c_program_with_args(
        &directory.0,
        "deny-kvm.so",
        PRCTL_DENY_KVM,
        &["-shared", "-fPIC", "-ldl"],
    );
    for test in [
        "native_and_kvm_prctl_names_keep_worker_local_and_format_procfs_leader_bytes",
        "kvm_direct_and_tool_match_prctl_identity_cell",
        "kvm_direct_and_tool_match_thp_disable_cell",
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--test-threads=1", "--nocapture"])
            .env("LD_PRELOAD", &library)
            .env("REVERIE_REQUIRE_KVM", "1")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(101),
            "required test {test}: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("requires usable /dev/kvm"),
            "{output:?}"
        );
    }
}
