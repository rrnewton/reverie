#!/usr/bin/env bash
# Copyright (c) Meta Platforms, Inc. and affiliates.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree.

# Measure the backend-maturity levels defined by the shared Reverie Tool
# contract.  Every row says what was compared; an exit status alone can never
# be mistaken for canonical Hermit evidence.

set -uo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
readonly ROOT_DIR
cd "$ROOT_DIR" || exit 1

# Check that known failures cannot become passing maturity rows.
python3 -I "$ROOT_DIR/scripts/test-backend-maturity.py" || exit 1

ARTIFACT_ROOT=${CARGO_TARGET_DIR:-"$ROOT_DIR/target"}
PROFILE=${BACKEND_MATURITY_PROFILE:-debug}
REPEATS=${BACKEND_MATURITY_REPEATS:-2}
CASE_TIMEOUT=${BACKEND_MATURITY_TIMEOUT_SECONDS:-300}
REPORT=${BACKEND_MATURITY_REPORT:-"$ARTIFACT_ROOT/backend-maturity.tsv"}
FULL_BACKENDS='ptrace kvm dbt sabre liteinst e9patch'
BACKENDS=${BACKEND_MATURITY_BACKENDS:-"$FULL_BACKENDS"}
SKIP_RELEASE_BUILD=${BACKEND_MATURITY_SKIP_RELEASE_BUILD:-0}
SKIP_PREPARE=${BACKEND_MATURITY_SKIP_PREPARE:-0}
readonly ARTIFACT_ROOT PROFILE CASE_TIMEOUT REPORT FULL_BACKENDS BACKENDS
readonly SKIP_RELEASE_BUILD SKIP_PREPARE
normalize_repeats() {
    python3 -I - "$1" <<'PY'
import re
import sys

value = sys.argv[1]
if not re.fullmatch(r"[0-9]+", value) or not 2 <= int(value) <= sys.maxsize - 1:
    raise SystemExit("BACKEND_MATURITY_REPEATS must be a decimal integer of at least 2 within the shell arithmetic range")
print(int(value))
PY
}

REPEATS=$(normalize_repeats "$REPEATS") || exit 2
readonly REPEATS
if [[ $PROFILE != debug ]]; then
    printf 'BACKEND_MATURITY_PROFILE must be debug; got %q\n' "$PROFILE" >&2
    exit 2
fi
if [[ $BACKENDS == "$FULL_BACKENDS" ]]; then
    PARTIAL_SELECTION=0
else
    PARTIAL_SELECTION=1
fi
readonly PARTIAL_SELECTION
read -r -a BACKEND_LIST <<<"$BACKENDS"
readonly BACKEND_LIST
for backend in "${BACKEND_LIST[@]}"; do
    case "$backend" in
        ptrace|kvm|dbt|sabre|liteinst|e9patch) ;;
        *)
            printf 'unknown backend in BACKEND_MATURITY_BACKENDS: %q\n' "$backend" >&2
            exit 2
            ;;
    esac
done

mkdir -p "$(dirname -- "$REPORT")" || exit 2
mkdir -p "$ARTIFACT_ROOT" || exit 2
# Keep generated build scripts and fixtures under the writable checkout. Some
# host execution policies permit file creation in /tmp but refuse to execute a
# newly built child from there, which is an infrastructure result rather than a
# backend result.
WORK_DIR=$(mktemp -d "$ARTIFACT_ROOT/backend-maturity-work.XXXXXX") || exit 2
WORK_DIR=$(cd -- "$WORK_DIR" && pwd) || exit 2
RELEASE_TARGET="$WORK_DIR/release-target"
TARGET_DIR="$WORK_DIR/runtime-target"
mkdir -- "$TARGET_DIR" "$RELEASE_TARGET" "$WORK_DIR/artifacts" "$WORK_DIR/tools" || exit 2
export CARGO_TARGET_DIR="$TARGET_DIR"
readonly WORK_DIR RELEASE_TARGET TARGET_DIR

source_snapshot() {
    python3 -I - "$ROOT_DIR" "$1" <<'PY'
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
def git(*args):
    return subprocess.check_output(["git", "-C", str(root), *args])
entries = []
paths = set()
for entry in git("ls-files", "--stage", "-z").split(b"\0"):
    if not entry:
        continue
    metadata, raw_path = entry.split(b"\t", 1)
    mode, oid, stage = metadata.decode().split()
    assert stage == "0", "unmerged source index"
    path = os.fsdecode(raw_path)
    paths.add(path)
    if mode == "160000":
        submodule = root / path
        record = dict(path=path, mode=mode, object=oid, initialized=(submodule / ".git").exists())
        if record["initialized"]:
            record["actual_head"] = subprocess.check_output(["git", "-C", str(submodule), "rev-parse", "HEAD"]).decode().strip()
            assert record["actual_head"] == oid
            assert not subprocess.check_output(["git", "-C", str(submodule), "status", "--porcelain=v1"])
        entries.append(record)
        continue
    item = root / path
    content = os.fsencode(os.readlink(item)) if item.is_symlink() else item.read_bytes()
    entries.append(dict(path=path, index_mode=mode, mode=item.lstat().st_mode, sha256=hashlib.sha256(content).hexdigest()))
for raw_path in git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0"):
    if raw_path:
        path = os.fsdecode(raw_path)
        if path not in paths:
            item = root / path
            content = os.fsencode(os.readlink(item)) if item.is_symlink() else item.read_bytes()
            entries.append(dict(path=path, untracked=True, mode=item.lstat().st_mode, sha256=hashlib.sha256(content).hexdigest()))
lock = root / "Cargo.lock"
assert lock.is_file() and not lock.is_symlink(), "a regular compatible Cargo.lock is required"
record = dict(head=git("rev-parse", "HEAD").decode().strip(), tree=git("rev-parse", "HEAD^{tree}").decode().strip(),
              lock_sha256=hashlib.sha256(lock.read_bytes()).hexdigest(), entries=entries)
encoded = (json.dumps(record, sort_keys=True, indent=2) + "\n").encode()
Path(sys.argv[2]).write_bytes(encoded)
print(hashlib.sha256(encoded).hexdigest())
PY
}

verify_source() {
    local digest
    digest=$(source_snapshot "$WORK_DIR/$1.source.json") || exit 2
    if [[ $digest != "$SOURCE_TREE_SHA256" ]]; then
        printf 'Source or Cargo.lock changed during maturity measurement: %s\n' "$1" >&2
        exit 2
    fi
}

REPOSITORY_SHA=$(git rev-parse HEAD)
if [[ -n $(git status --porcelain --untracked-files=all) ]]; then
    TREE_DIRTY=1
else
    TREE_DIRTY=0
fi
SOURCE_TREE_SHA256=$(source_snapshot "$WORK_DIR/source.json") || exit 2
RUST_TOOLCHAIN=$(rustc --version) || exit 2
readonly SOURCE_TREE_SHA256 RUST_TOOLCHAIN
if ! {
    printf '# repository_sha\t%s\n' "$REPOSITORY_SHA"
    printf '# tree_dirty\t%s\n' "$TREE_DIRTY"
    printf '# partial_selection\t%s\n' "$PARTIAL_SELECTION"
    printf '# skipped_runtime_preparation\t%s\n' "$SKIP_PREPARE"
    printf '# source_tree_sha256\t%s\n' "$SOURCE_TREE_SHA256"
    printf '# rust_toolchain\t%s\n' "$RUST_TOOLCHAIN"
    printf '# retained_evidence\t%s\n' "$WORK_DIR"
    printf '# fresh_runtime_target\t%s\n' "$TARGET_DIR"
    printf '# retention_limit\tInternal test-generated/rewrite guest ELFs and internally captured successful diagnostics are not exported; this is not a reconstructable archive of all executed bytes.\n'
    printf 'backend\tlevel\toutcome\trepeats\texit_status\tguest_stdout\ttool_output\tprocess_thread_accounting\tbitwise_parity\tcanonical_info\tpositive_info_counts\tfull_corpus\tevidence\tdetail\n'
} >"$REPORT"; then
    printf 'Cannot write maturity report: %s\n' "$REPORT" >&2
    exit 2
fi
cp -- "$REPORT" "$WORK_DIR/report.tsv" || exit 2

declare -A LEVEL_OUTCOME=()
declare -A MAXIMUM_LEVEL=()
declare -A CASE_INFRASTRUCTURE_DENIED=()
declare -A CASE_EXECUTION_FAILED=()
declare -A CASE_DEPENDENCY=()
ARTIFACTS_SEALED=0
declare -A MINIMUM_LEVEL=(
    [ptrace]=B1.5
    [kvm]=B1.5
    [dbt]=B1
    [sabre]=B1.5
    [liteinst]=B1.5
    [e9patch]=B1.5
)

sanitize() {
    local value=$1
    value=${value//$'\t'/ }
    value=${value//$'\n'/ }
    value=${value//$'\r'/ }
    printf '%s' "$value"
}

record() {
    local backend=$1 level=$2 outcome=$3 repeats=$4
    local exit_status=$5 guest_stdout=$6 tool_output=$7 process_threads=$8
    local bitwise=$9 canonical=${10} info_counts=${11} full_corpus=${12}
    local evidence=${13} detail=${14}

    if [[ $REPORT != "$WORK_DIR/report.tsv" && -f $WORK_DIR/report.tsv ]] && ! cmp -s "$REPORT" "$WORK_DIR/report.tsv"; then
        printf 'Maturity report changed outside this run: %s\n' "$REPORT" >&2
        exit 2
    fi
    if ! printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$backend" "$level" "$outcome" "$repeats" "$exit_status" \
        "$guest_stdout" "$tool_output" "$process_threads" "$bitwise" \
        "$canonical" "$info_counts" "$full_corpus" \
        "$(sanitize "$evidence")" "$(sanitize "$detail")" >>"$REPORT"; then
        printf 'Cannot append maturity report: %s\n' "$REPORT" >&2
        exit 2
    fi
    if [[ $REPORT != "$WORK_DIR/report.tsv" ]]; then
        cp -- "$REPORT" "$WORK_DIR/report.tsv" || exit 2
    fi
    LEVEL_OUTCOME["$backend:$level"]=$outcome
    printf 'backend=%-8s level=%-4s outcome=%-12s compared=%s detail=%s\n' \
        "$backend" "$level" "$outcome" "$evidence" "$detail"
}

run_case() {
    local label=$1
    shift
    if [[ ! $label =~ ^[a-zA-Z0-9._-]+$ || -e $WORK_DIR/$label.command.json ]]; then
        printf 'Invalid or previously recorded case label: %s\n' "$label" >&2
        exit 2
    fi
    CURRENT_CASE=$label
    local status denied
    if [[ -n ${SOURCE_TREE_SHA256:-} ]]; then
        verify_source "$label-before"
    fi
    if [[ -n ${BUILD_CONTEXT_SHA256:-} ]]; then
        verify_build_context "$label-before"
    fi
    if [[ ${ARTIFACTS_SEALED:-0} == 1 ]]; then
        verify_bound_artifacts || exit 2
    fi
    python3 -I - "$WORK_DIR/$label" "$CASE_TIMEOUT" "${SOURCE_TREE_SHA256:-}" "$@" <<'PY' || exit 2
import ctypes
import errno
import json
import math
import os
import signal
import shutil
import subprocess
import sys
import time
from pathlib import Path

prefix, timeout, source_sha256, *command = sys.argv[1:]
started = time.monotonic()
record = dict(command=command, cwd=os.getcwd(), launched=False, source_manifest_sha256=source_sha256)
record["environment"] = {name: value for name, value in os.environ.items() if name in (
    "PATH", "CARGO_TARGET_DIR", "CARGO_BUILD_JOBS", "CMAKE_BUILD_PARALLEL_LEVEL", "CC", "CXX", "CMAKE",
    "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTUP_TOOLCHAIN", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
    "REVERIE_REQUIRE_KVM", "REVERIE_E9TOOL", "REVERIE_E9PATCH_BACKEND", "LD_PRELOAD", "LD_LIBRARY_PATH")}
record["resolved_launcher"] = shutil.which(command[0])
interrupted = []
def received_signal(number, _frame):
    interrupted.append(number)
for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
    signal.signal(number, received_signal)
Path(prefix + ".stdout").touch()
Path(prefix + ".stderr").touch()
try:
    timeout_seconds = float(timeout)
    if not math.isfinite(timeout_seconds) or timeout_seconds <= 0:
        raise ValueError("timeout must be finite and positive")
except ValueError as error:
    record.update(exit_status=2, launch_denied=False, configuration_error=str(error))
    Path(prefix + ".command.json").write_text(json.dumps(record, indent=2) + "\n")
    Path(prefix + ".status").write_text("2")
    Path(prefix + ".denied").write_text("0")
    raise SystemExit(0)
try:
    fields = Path("/proc/self/status").read_text().splitlines()
    process_id = int(next(line for line in fields if line.startswith("Pid:")).split()[1])
    if process_id != os.getpid():
        raise OSError("procfs must use the case supervisor's PID namespace")
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)
    library = ctypes.CDLL(None, use_errno=True)
    library.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
    library.prctl.restype = ctypes.c_int
    if library.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER failed")
    capability = os.pidfd_open(os.getpid())
    os.close(capability)
    if not hasattr(signal, "pidfd_send_signal"):
        raise OSError("pidfd_send_signal is required")
except (AttributeError, OSError) as error:
    record.update(exit_status=2, launch_denied=False, configuration_error=str(error))
    Path(prefix + ".command.json").write_text(json.dumps(record, indent=2) + "\n")
    Path(prefix + ".status").write_text("2")
    Path(prefix + ".denied").write_text("0")
    raise SystemExit(0)
status, denied = 127, 0
with open(prefix + ".stdout", "wb") as stdout, open(prefix + ".stderr", "wb") as stderr:
    try:
        child = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
    except OSError as error:
        record.update(launch_errno=error.errno, launch_error=str(error))
        denied = int(error.errno in (errno.EACCES, errno.EPERM, errno.EAGAIN))
        status = 126 if denied else 127
        stderr.write((str(error) + "\n").encode())
    else:
        record["launched"] = True
        record["command_pid"] = child.pid
        record["children"] = []
        record["signals"] = []
        def reap_children():
            while True:
                try:
                    pid, wait_status = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    return True
                if pid == 0:
                    return False
                exit_status = os.waitstatus_to_exitcode(wait_status)
                record["children"].append(dict(pid=pid, wait_status=wait_status, exit_status=exit_status, command=(pid == child.pid)))
                if pid == child.pid:
                    child.returncode = exit_status
        def signal_children(number):
            identities = set()
            for process in Path("/proc").iterdir():
                if not process.name.isdecimal():
                    continue
                try:
                    fields = (process / "status").read_text().splitlines()
                except FileNotFoundError:
                    continue
                parent = int(next(line for line in fields if line.startswith("PPid:")).split()[1])
                if parent == os.getpid():
                    identities.add(int(process.name))
            for pid in identities:
                try:
                    descriptor = os.pidfd_open(pid)
                except ProcessLookupError:
                    continue
                try:
                    fields = Path(f"/proc/{pid}/status").read_text().splitlines()
                    parent = int(next(line for line in fields if line.startswith("PPid:")).split()[1])
                    if parent == os.getpid():
                        signal.pidfd_send_signal(descriptor, number)
                        record["signals"].append(dict(pid=pid, signal=number))
                except (ProcessLookupError, FileNotFoundError):
                    pass
                finally:
                    os.close(descriptor)
        deadline = time.monotonic() + timeout_seconds
        complete = reap_children()
        while not complete and not interrupted and time.monotonic() < deadline:
            time.sleep(min(0.01, max(0.001, deadline-time.monotonic())))
            complete = reap_children()
        if not complete:
            status = 128 + interrupted[0] if interrupted else 124
            record["interrupted_signal"] = interrupted[0] if interrupted else None
            record["timed_out"] = not interrupted
            try:
                for number in (signal.SIGTERM, signal.SIGKILL):
                    cleanup_deadline = time.monotonic() + 5
                    while not complete and time.monotonic() < cleanup_deadline:
                        signal_children(number)
                        time.sleep(0.01)
                        complete = reap_children()
                    if complete:
                        break
            except OSError as error:
                record["cleanup_error"] = str(error)
        else:
            status = child.returncode
        record["command_exit_status"] = child.returncode
        record["cleanup_complete"] = complete
        if status < 0:
            status = 128 - status
record.update(exit_status=status, launch_denied=bool(denied), elapsed_seconds=time.monotonic()-started)
Path(prefix + ".command.json").write_text(json.dumps(record, indent=2) + "\n")
Path(prefix + ".status").write_text(str(status))
Path(prefix + ".denied").write_text(str(denied))
PY
    if ! python3 -I - "$WORK_DIR/$label.command.json" <<'PY'
import json
import sys
from pathlib import Path
record = json.loads(Path(sys.argv[1]).read_text())
raise SystemExit(0 if not record["launched"] or record.get("cleanup_complete") is True else 1)
PY
    then
        printf 'Case cleanup remains incomplete; stopping: %s\n' "$label" >&2
        exit 2
    fi
    if [[ -n ${SOURCE_TREE_SHA256:-} ]]; then
        verify_source "$label-after"
    fi
    if [[ -n ${BUILD_CONTEXT_SHA256:-} ]]; then
        verify_build_context "$label-after"
    fi
    if [[ ${ARTIFACTS_SEALED:-0} == 1 ]]; then
        verify_bound_artifacts || exit 2
    fi
    status=$(<"$WORK_DIR/$label.status")
    denied=$(<"$WORK_DIR/$label.denied")
    if [[ -n ${SOURCE_TREE_SHA256:-} && $status == 0 && ( $label == prepare-* || $label == b0-* || $label == compile-* ) ]]; then
        capture_build_outputs "$label" || exit 2
    fi
    declare -gA CASE_EXECUTION_FAILED
    CASE_EXECUTION_FAILED["$label"]=$((status != 0 && denied == 0))
    CASE_INFRASTRUCTURE_DENIED["$label"]=$denied
    return "$status"
}

build_context() {
    python3 -I - "$ROOT_DIR" "$WORK_DIR" "$1" <<'PY'
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

root, work, output = map(Path, sys.argv[1:])
records = {}
def retain(filename):
    path = Path(filename)
    resolved = path.resolve(strict=True)
    assert resolved.is_file()
    with resolved.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    copy = work / "tools" / (digest + "-" + str(resolved.stat().st_mode))
    if not copy.exists():
        shutil.copy2(resolved, copy)
    with copy.open("rb") as stream:
        assert hashlib.file_digest(stream, "sha256").hexdigest() == digest
    assert copy.stat().st_mode == resolved.stat().st_mode
    records[str(path)] = dict(resolved=str(resolved), sha256=digest, mode=resolved.stat().st_mode)
    return str(resolved)
def resolve(name):
    path = shutil.which(name)
    assert path, f"required tool not found: {name}"
    return retain(path)
environment = {name: value for name, value in os.environ.items()
               if name.startswith(("CARGO_", "RUST", "REVERIE_", "CMAKE_", "CC_", "CXX_", "AR_", "LD_"))
               or name in ("PATH", "HOME", "CC", "CXX", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "AR", "LD", "CMAKE", "PKG_CONFIG_PATH", "LIBRARY_PATH", "CPATH", "TMPDIR", "PROFILE")}
for name in ("RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_ENCODED_RUSTFLAGS",
             "CARGO_ENCODED_RUSTDOCFLAGS", "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"):
    assert not os.environ.get(name), f"unqualified compiler override: {name}"
rustflags = os.environ.get("RUSTFLAGS", "").split()
rustdocflags = os.environ.get("RUSTDOCFLAGS", "").split()
lzma_link = None
if rustflags or rustdocflags:
    assert rustdocflags == ["-D", "warnings"], "unqualified rustdoc warning flags"
    assert len(rustflags) == 4 and rustflags[:3] == ["-D", "warnings", "-C"], "unqualified rustc flags"
    assert rustflags[3].startswith("link-arg="), "missing driver link argument"
    target = rustflags[3].removeprefix("link-arg=")
    if target == "-llzma":
        command = [resolve("cc"), "-print-file-name=liblzma.so"]
        lookup = subprocess.run(command, check=True, capture_output=True, text=True, timeout=15)
        selected = lookup.stdout.strip()
        assert selected != "liblzma.so" and Path(selected).is_file(), "driver lzma library not found"
    else:
        assert Path(target).is_absolute() and re.fullmatch(r"liblzma\.so\.[0-9]+", Path(target).name), "unqualified driver link target"
        command = [resolve("ldconfig"), "-p"]
        lookup = subprocess.run(command, check=True, capture_output=True, text=True, timeout=15)
        candidates = [line.split()[-1] for line in lookup.stdout.splitlines()
                      if re.match(r"^\s*liblzma\.so\.[0-9]+\s", line)]
        assert candidates and target == candidates[0], "link target differs from driver library selection"
        selected = target
    resolved = retain(selected)
    retain(resolved)
    with Path(resolved).open("rb") as stream:
        assert stream.read(4) == b"\x7fELF", "driver lzma dependency is not an ELF"
    dynamic = subprocess.run([resolve("readelf"), "-dW", resolved], check=True, capture_output=True, text=True, timeout=15)
    soname = re.findall(r"\(SONAME\).*\[([^\]]+)\]", dynamic.stdout)
    assert len(soname) == 1 and re.fullmatch(r"liblzma\.so\.[0-9]+", soname[0]), "driver dependency has the wrong SONAME"
    lzma_link = dict(target=target, selected=selected, resolved=resolved, soname=soname[0],
                     lookup_command=command, lookup_stdout=lookup.stdout, lookup_stderr=lookup.stderr,
                     lookup_status=lookup.returncode, dynamic_stdout=dynamic.stdout)
environment = {name: value for name, value in environment.items() if not any(secret in name for secret in ("TOKEN", "PASSWORD", "CREDENTIAL"))
               and name not in ("REVERIE_E9TOOL", "REVERIE_E9PATCH_BACKEND")}
configuration = []
cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
for config_dir in [directory / ".cargo" for directory in [root, *root.parents]] + [cargo_home]:
    for basename in ("config", "config.toml"):
        path = config_dir / basename
        if path.is_file() and str(path) not in configuration:
            configuration.append(str(path))
            retain(path)
            settings = tomllib.loads(path.read_text())
            build = settings.get("build", {})
            assert not any(build.get(name) for name in ("rustc", "rustc-wrapper", "rustc-workspace-wrapper", "rustflags", "target")), "unqualified Cargo build override"
            for target in settings.get("target", {}).values():
                assert not target.get("rustflags"), "unqualified target rustflags"
                if target.get("linker"):
                    resolve(target["linker"])
tools = ["rustup", "cargo", "rustc", "bash", "env", "python3", "git", "make", "ar", "ld", "as", "readelf", "ldd", "pkg-config",
         os.environ.get("CC", "cc"), os.environ.get("CXX", "c++"), os.environ.get("CMAKE", "cmake"), "/bin/echo", "/bin/sh", "/bin/true"]
resolved_tools = {name: resolve(name) for name in tools}
toolchain_file = root / "rust-toolchain.toml"
retain(toolchain_file)
channel = tomllib.loads(toolchain_file.read_text())["toolchain"]["channel"]
installed = subprocess.run(["rustup", "toolchain", "list"], check=True, capture_output=True, text=True, timeout=15)
assert any(line.split()[0] == channel or line.split()[0].startswith(channel + "-") for line in installed.stdout.splitlines()), "declared toolchain must already be installed"
for name in ("rustc", "cargo"):
    result = subprocess.run(["rustup", "which", name], check=True, capture_output=True, text=True, timeout=15)
    resolved_tools["active-" + name] = retain(result.stdout.strip())
    assert resolved_tools[name] in (resolved_tools["rustup"], resolved_tools["active-" + name]), f"unqualified {name} launcher"
    declared = subprocess.run(["rustup", "which", "--toolchain", channel, name], check=True, capture_output=True, text=True, timeout=15)
    assert str(Path(declared.stdout.strip()).resolve(strict=True)) == resolved_tools["active-" + name], "active compiler differs from the source-declared toolchain"
versions = {}
for name in ("active-rustc", "active-cargo"):
    result = subprocess.run([resolved_tools[name], "-vV"], check=True, capture_output=True, timeout=15)
    versions[name] = dict(stdout=result.stdout.decode(), stderr=result.stderr.decode(), status=result.returncode)
for name in (os.environ.get("CC", "cc"), os.environ.get("CXX", "c++")):
    compiler = resolved_tools[name]
    with Path(compiler).open("rb") as stream:
        assert stream.read(4) == b"\x7fELF", "native compiler wrappers are not qualified by this predicate"
    for program in ("cc1", "cc1plus", "collect2", "as", "ld"):
        result = subprocess.run([compiler, "-print-prog-name=" + program], check=True, capture_output=True, text=True, timeout=15)
        value = result.stdout.strip()
        if Path(value).is_file():
            retain(value)
        elif value == program and shutil.which(value):
            resolve(value)
    result = subprocess.run([compiler, "--version"], check=True, capture_output=True, timeout=15)
    versions[name] = dict(stdout=result.stdout.decode(), stderr=result.stderr.decode(), status=result.returncode)
for filename in list(records):
    with Path(filename).open("rb") as stream:
        elf = stream.read(4) == b"\x7fELF"
    if not elf:
        continue
    program = records[filename]["resolved"]
    headers = subprocess.run([resolved_tools["readelf"], "-lW", program], check=True, capture_output=True, text=True, timeout=15)
    interpreter = re.search(r"Requesting program interpreter: ([^\]]+)\]", headers.stdout)
    if not interpreter:
        continue
    loader = retain(interpreter[1])
    result = subprocess.run([loader, "--list", program], capture_output=True, text=True, timeout=15)
    assert "not found" not in result.stdout + result.stderr, (filename, result.stdout, result.stderr)
    assert result.returncode == 0, (filename, result.stdout, result.stderr)
    for line in result.stdout.splitlines():
        match = re.search(r"(?:=>\s+|^\s*)(/[^\s]+)", line)
        if match:
            retain(match[1])
record = dict(environment=environment, files=records, versions=versions, configuration=configuration, lzma_link=lzma_link,
              internal_test_guest_archive=False)
encoded = (json.dumps(record, indent=2, sort_keys=True) + "\n").encode()
output.write_bytes(encoded)
print(hashlib.sha256(encoded).hexdigest())
PY
}

verify_build_context() {
    local current
    current=$(build_context "$WORK_DIR/context-$1.json") || exit 2
    if [[ $current != "$BUILD_CONTEXT_SHA256" ]]; then
        printf 'Compiler, dependency, configuration or environment changed during maturity validation\n' >&2
        exit 2
    fi
}

capture_build_outputs() {
    python3 -I - "$WORK_DIR" "$1" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

work, label = Path(sys.argv[1]), sys.argv[2]
records = {}
roots = [work / "runtime-target", work / "release-target"]
paths = [path for root in roots if root.exists() for path in root.rglob("*")]
paths.extend(path for path in work.iterdir() if path.is_file() and path.suffix == "")
for path in paths:
    if not path.is_file():
        continue
    resolved = path.resolve(strict=True)
    resolved.relative_to(work.resolve())
    with resolved.open("rb") as stream:
        sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    records[str(resolved)] = dict(sha256=sha256, mode=resolved.stat().st_mode)
(work / (label + ".outputs.json")).write_text(json.dumps(records, indent=2, sort_keys=True) + "\n")
PY
}

bind_artifact() {
    local path=$1 producer=$2
    python3 -I - "$WORK_DIR" "$path" "$producer" "$SOURCE_TREE_SHA256" <<'PY'
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

work = Path(sys.argv[1]).resolve()
path = Path(sys.argv[2]).absolute()
resolved = path.resolve(strict=True)
resolved.relative_to(work)
assert path.is_file()
producer, source = sys.argv[3:]
command = json.loads((work / (producer + ".command.json")).read_text())
assert command["exit_status"] == 0 and command["launched"]
assert command.get("cleanup_complete") is True, "producing command did not finish child cleanup"
assert command["source_manifest_sha256"] == source
def digest(item):
    with item.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()
sha256 = digest(path)
outputs = json.loads((work / (producer + ".outputs.json")).read_text())
assert outputs[str(resolved)] == dict(sha256=sha256, mode=path.stat().st_mode), f"artifact changed after producing command: {producer}: {path}"
retained = work / "artifacts" / (sha256 + "-" + str(path.stat().st_mode))
if not retained.exists():
    shutil.copy2(path, retained)
assert digest(retained) == sha256 == digest(path)
registry = work / "artifact-registry.json"
records = json.loads(registry.read_text()) if registry.exists() else {}
if str(path) in records:
    assert records[str(path)]["sha256"] == sha256 and records[str(path)]["mode"] == path.stat().st_mode, "previously bound artifact changed"
records[str(path)] = dict(path=str(path), resolved=str(resolved), sha256=sha256, bytes=path.stat().st_size,
                          mode=path.stat().st_mode, producer=producer, source_manifest_sha256=source,
                          retained=str(retained))
temporary = registry.with_suffix(".new")
temporary.write_text(json.dumps(records, indent=2, sort_keys=True) + "\n")
os.replace(temporary, registry)
PY
}

verify_bound_artifacts() {
    python3 -I - "$WORK_DIR/artifact-registry.json" "$SOURCE_TREE_SHA256" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

records = json.loads(Path(sys.argv[1]).read_text())
assert records, "no bound artifacts"
entries = list(records.values())
dependencies = Path(sys.argv[1]).with_name("runtime-dependencies.json")
if dependencies.exists():
    entries.extend(json.loads(dependencies.read_text()).values())
for record in entries:
    assert record["source_manifest_sha256"] == sys.argv[2]
    path = Path(record["path"])
    assert str(path.resolve(strict=True)) == record.get("resolved", record["path"])
    assert path.stat().st_mode == record["mode"] and path.stat().st_size == record["bytes"]
    for item in (path, Path(record["retained"])):
        assert item.stat().st_mode == record["mode"]
        with item.open("rb") as stream:
            assert hashlib.file_digest(stream, "sha256").hexdigest() == record["sha256"], str(item)
PY
}

retain_runtime_dependencies() {
    python3 -I - "$WORK_DIR" "$SOURCE_TREE_SHA256" <<'PY'
import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

work, source = Path(sys.argv[1]), sys.argv[2]
registry = json.loads((work / "artifact-registry.json").read_text())
outputs = {path.stem.removesuffix(".outputs"): json.loads(path.read_text()) for path in work.glob("prepare-*.outputs.json")}
records, commands = {}, []
headers = subprocess.run(["readelf", "-lW", str(Path("/bin/sh").resolve())], check=True, capture_output=True, text=True, timeout=15)
match = re.search(r"Requesting program interpreter: ([^\]]+)\]", headers.stdout)
assert match
host_loader = match[1]
def retain(filename):
    path = Path(filename).resolve(strict=True)
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    mode = path.stat().st_mode
    origin = "host loader dependency"
    if path.is_relative_to(work):
        origin = [label for label, paths in outputs.items() if paths.get(str(path)) == dict(sha256=digest, mode=mode)]
        assert origin, f"dependency not present with these bytes after a current preparation command: {path}"
    retained = work / "artifacts" / (digest + "-" + str(mode))
    if not retained.exists():
        shutil.copy2(path, retained)
    records[str(path)] = dict(path=str(path), sha256=digest, bytes=path.stat().st_size, mode=mode,
                              retained=str(retained), source_manifest_sha256=source, origin=origin)
for record in registry.values():
    path = Path(record["path"])
    with path.open("rb") as stream:
        if stream.read(4) != b"\x7fELF":
            continue
    headers = subprocess.run(["readelf", "-lW", str(path)], check=True, capture_output=True, text=True, timeout=15)
    interpreter = re.search(r"Requesting program interpreter: ([^\]]+)\]", headers.stdout)
    dynamic = subprocess.run(["readelf", "-dW", str(path)], check=True, capture_output=True, text=True, timeout=15)
    if not interpreter and "(NEEDED)" not in dynamic.stdout:
        continue
    loader = interpreter[1] if interpreter else host_loader
    retain(loader)
    command = [loader, "--list", str(path)]
    result = subprocess.run(command, capture_output=True, text=True, timeout=15)
    commands.append(dict(command=command, status=result.returncode, stdout=result.stdout, stderr=result.stderr))
    (work / "runtime-dependency-commands.json").write_text(json.dumps(commands, indent=2) + "\n")
    assert result.returncode == 0 and "not found" not in result.stdout + result.stderr
    for line in result.stdout.splitlines():
        match = re.search(r"(?:=>\s+|^\s*)(/[^\s]+)", line)
        if match:
            retain(match[1])
(work / "runtime-dependencies.json").write_text(json.dumps(records, indent=2, sort_keys=True) + "\n")
PY
}

bind_cargo_outputs() {
    local label=$1 path
    python3 -I - "$WORK_DIR" "$TARGET_DIR" "$label" <<'PY' || return
import json
import sys
from pathlib import Path

work, target, label = Path(sys.argv[1]), Path(sys.argv[2]).resolve(), sys.argv[3]
messages = [json.loads(line) for line in (work / (label + ".stdout")).read_text().splitlines()]
finished = [item for item in messages if item.get("reason") == "build-finished"]
assert len(finished) == 1 and finished[0]["success"]
paths = set()
for message in messages:
    if message.get("reason") == "compiler-artifact":
        for filename in message["filenames"]:
            path = Path(filename).absolute()
            path.resolve(strict=True).relative_to(target)
            assert path.is_file()
            paths.add(str(path))
assert paths
(work / (label + ".paths")).write_text("\n".join(sorted(paths)) + "\n")
PY
    while IFS= read -r path; do
        bind_artifact "$path" "$label" || return
    done <"$WORK_DIR/$label.paths"
}

check_test_preload() {
    local label=$1 target=$2 binary preload path
    binary=$(test_binary_from_case "$label" "$target") || return
    preload=$(cargo_artifact "$label" reverie_examples .so reverie-examples) || return
    python3 -I - "$binary" "$preload" "$WORK_DIR/$label.preloads" <<'PY' || return
import hashlib
import sys
from pathlib import Path

binary, preload, output = map(Path, sys.argv[1:])
def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()
expected = digest(preload)
parent = binary.parent
paths = [parent / "libreverie_examples.so", parent / "../libreverie_examples.so"]
if binary.name.startswith("e9patch_direct-"):
    paths.append(parent / "../../libreverie_examples.so")
found = [path.absolute() for path in paths if path.is_file()]
assert found
assert all(digest(path) == expected for path in found), "test preload choices differ from emitted DSO"
output.write_text("\n".join(map(str, found)) + "\n")
PY
    while IFS= read -r path; do
        bind_artifact "$path" "$label" || return
    done <"$WORK_DIR/$label.preloads"
}

cargo_artifact() {
    local label=$1 target=$2 selection=$3 package=$4 path
    path=$(python3 -I - "$WORK_DIR/$label.stdout" "$target" "$selection" "$WORK_DIR" "$package" <<'PY'
import json
import sys
from pathlib import Path

log, target, selection, work, package = sys.argv[1:]
metadata = json.loads((Path(work) / "metadata.stdout").read_text())
identities = [item["id"] for item in metadata["packages"] if item["name"] == package]
assert len(identities) == 1
messages = [json.loads(line) for line in Path(log).read_text().splitlines()]
finished = [message for message in messages if message.get("reason") == "build-finished"]
assert len(finished) == 1 and finished[0]["success"]
candidates = set()
for message in messages:
    if message.get("reason") != "compiler-artifact" or message.get("target", {}).get("name") != target or message.get("package_id") != identities[0]:
        continue
    if selection in ("executable", "test"):
        if selection == "test":
            assert message["profile"]["test"] and "test" in message["target"]["kind"]
        else:
            assert "bin" in message["target"]["kind"]
        values = [message.get("executable")]
    else:
        values = [value for value in message["filenames"] if value.endswith(selection)]
    for value in values:
        if value:
            path = Path(value).absolute()
            path.resolve(strict=True).relative_to(Path(work).resolve())
            assert path.is_file()
            candidates.add(str(path))
assert len(candidates) == 1, f"missing or ambiguous emitted artifact: {target} {selection}"
print(candidates.pop())
PY
    ) || return
    bind_artifact "$path" "$label" || return
    printf '%s\n' "$path"
}

verify_release_build() {
    local label=$1 package=$2 path
    python3 -I - "$WORK_DIR" "$RELEASE_TARGET" "$label" "$package" <<'PY' || return
import json
import sys
from pathlib import Path

work, target, label, package = sys.argv[1:]
metadata = json.loads((Path(work) / "metadata.stdout").read_text())
ids = [item["id"] for item in metadata["packages"] if item["name"] == package]
assert len(ids) == 1
messages = [json.loads(line) for line in (Path(work) / (label + ".stdout")).read_text().splitlines()]
finished = [item for item in messages if item.get("reason") == "build-finished"]
assert len(finished) == 1 and finished[0]["success"]
artifacts = [item for item in messages if item.get("reason") == "compiler-artifact" and item.get("package_id") == ids[0]]
assert artifacts and any(item["fresh"] is False for item in artifacts), "no fresh build of selected package"
paths = set()
for item in artifacts:
    for name in item["filenames"]:
        path = Path(name).resolve(strict=True)
        path.relative_to(Path(target).resolve())
        assert path.is_file()
        paths.add(str(path))
assert paths
(Path(work) / (label + ".artifacts")).write_text("\n".join(sorted(paths)) + "\n")
PY
    while IFS= read -r path; do
        bind_artifact "$path" "$label" || return
    done <"$WORK_DIR/$label.artifacts"
}

native_build_artifact() {
    local label=$1 package=$2 relative=$3 path
    path=$(python3 -I - "$WORK_DIR/$label.stdout" "$ROOT_DIR/$package" "$relative" "$TARGET_DIR" <<'PY'
import json
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

log, package, relative, target = sys.argv[1:]
paths = set()
messages = [json.loads(line) for line in Path(log).read_text().splitlines()]
finished = [message for message in messages if message.get("reason") == "build-finished"]
assert len(finished) == 1 and finished[0]["success"]
for message in messages:
    if message.get("reason") != "build-script-executed":
        continue
    origin = message["package_id"].partition("#")[0].removeprefix("path+")
    if Path(unquote(urlsplit(origin).path)).resolve() != Path(package).resolve():
        continue
    path = (Path(message["out_dir"]) / relative).resolve(strict=True)
    path.relative_to(Path(target).resolve())
    paths.add(str(path))
assert len(paths) == 1, f"missing or ambiguous native build output: {package}"
print(paths.pop())
PY
    ) || return
    bind_artifact "$path" "$label" || return
    printf '%s\n' "$path"
}

exact_stdout() {
    local path=$1 expected=$2
    printf '%s\n' "$expected" >"$path.expected" || exit 2
    if ! cmp -s "$path" "$path.expected"; then
        comparison_failed
        return 1
    fi
}

comparison_failed() {
    local label=${CURRENT_CASE:-}
    if [[ -n $label && -f $WORK_DIR/$label.status && $(<"$WORK_DIR/$label.status") == 0 ]]; then
        CASE_EXECUTION_FAILED["$label"]=1
    fi
}

case_status() {
    local status=not-executed
    if [[ -f $WORK_DIR/$1.status ]]; then
        status=$(<"$WORK_DIR/$1.status")
    fi
    printf '%s' "$status"
}

infrastructure_denied() {
    [[ ${CASE_INFRASTRUCTURE_DENIED["$1"]:-0} == 1 ]]
}

any_infrastructure_denied() {
    local prefix=$1 label
    for label in "${!CASE_INFRASTRUCTURE_DENIED[@]}"; do
        if [[ $label == "$prefix"* && ${CASE_INFRASTRUCTURE_DENIED[$label]} == 1 ]]; then
            return 0
        fi
    done
    return 1
}

any_execution_failed() {
    local prefix=$1 label
    for label in "${!CASE_EXECUTION_FAILED[@]}"; do
        if [[ $label == "$prefix"* && ${CASE_EXECUTION_FAILED[$label]} == 1 ]]; then
            return 0
        fi
    done
    return 1
}

record_runtime_failure() {
    local backend=$1 level=$2 label=$3 evidence=$4 detail=$5
    declare -gA CASE_DEPENDENCY
    label=${CASE_DEPENDENCY[$label]:-$label}
    local outcome=fail
    if infrastructure_denied "$label"; then
        outcome=unmeasurable
        detail="host execution policy denied the measurement; $detail"
    fi
    record "$backend" "$level" "$outcome" 0 compared compared missing \
        not_measured not_measured not_measured not_measured not_measured \
        "$evidence" "$detail (exit $(case_status "$label"))"
}

record_b15_failure() {
    local backend=$1 prefix=$2 evidence=$3 detail=$4 outcome=fail
    if ! any_execution_failed "$prefix" && any_infrastructure_denied "$prefix"; then
        outcome=unmeasurable
        detail="host execution policy denied at least one required command; $detail"
    fi
    record "$backend" B1.5 "$outcome" "$REPEATS" compared compared missing missing \
        not_measured not_measured not_measured not_measured "$evidence" "$detail"
}

measure_b0() {
    local backend=$1 package=$2 label="b0-$1"
    if [[ $TREE_DIRTY == 1 ]]; then
        record "$backend" B0 unmeasurable 0 not_measured not_applicable not_applicable \
            not_applicable not_measured not_measured not_measured not_measured \
            'clean release build exit status' \
            'source checkout is dirty, so the clean-checkout B0 prerequisite cannot be awarded'
        return
    fi
    if [[ $SKIP_RELEASE_BUILD == 1 ]]; then
        record "$backend" B0 unmeasurable 0 not_measured not_applicable not_applicable \
            not_applicable not_measured not_measured not_measured not_measured \
            'clean release build exit status' \
            'release build omitted by BACKEND_MATURITY_SKIP_RELEASE_BUILD=1'
        return
    fi
    if run_case "$label" cargo build --locked --release --message-format=json --target-dir "$RELEASE_TARGET" -p "$package" &&
        verify_release_build "$label" "$package"; then
        record "$backend" B0 pass 1 compared not_applicable not_applicable \
            not_applicable not_measured not_measured not_measured not_measured \
            'clean release build exit status' \
            "cargo build --release --target-dir <fresh-directory> -p $package; repository=$REPOSITORY_SHA; rustc=$RUST_TOOLCHAIN"
    else
        record_runtime_failure "$backend" B0 "$label" \
            'clean release build exit status' \
            "cargo build --release --target-dir <fresh-directory> -p $package"
    fi
}

compile_fixture() {
    local source=$1 output=$2 label
    label="compile-${output##*/}"
    run_case "$label" "${CC:-cc}" -O2 -g -std=c11 -Wall -Wextra -Werror "$source" -o "$output" || return
    bind_artifact "$output" "$label"
}

test_binary_from_case() {
    cargo_artifact "$1" "$2" test reverie-examples
}

verify_test_execution() {
    local label=$1 selected=$2 mode=$3
    python3 -I - "$WORK_DIR/$label-list.stdout" "$WORK_DIR/$label.stdout" "$selected" "$mode" <<'PY'
import json
import sys
from pathlib import Path

listing, output, selected, mode = sys.argv[1:]
discovery = [json.loads(line) for line in Path(listing).read_text().splitlines()]
results = [json.loads(line) for line in Path(output).read_text().splitlines()]
tests = [item for item in discovery if item.get("type") == "test" and item.get("event") == "discovered"]
names = [item["name"] for item in tests]
assert names and len(names) == len(set(names))
discovery_end = [item for item in discovery if item.get("type") == "suite" and item.get("event") == "completed"]
assert len(discovery_end) == 1 and discovery_end[0]["tests"] == len(tests)
assert discovery_end[0]["benchmarks"] == 0 and discovery_end[0]["total"] == len(tests)
if selected:
    expected = [item["name"] for item in tests if item["name"] == selected]
    assert expected == [selected]
    assert next(item["ignore"] for item in tests if item["name"] == selected) == (mode == "ignored")
else:
    assert mode == "normal" and all(not item["ignore"] for item in tests)
    expected = names
begun = [item["name"] for item in results if item.get("type") == "test" and item.get("event") == "started"]
finished = [item["name"] for item in results if item.get("type") == "test" and item.get("event") == "ok"]
assert sorted(begun) == sorted(expected) == sorted(finished)
assert all(item.get("event") in ("started", "ok") for item in results if item.get("type") == "test")
suites = [item for item in results if item.get("type") == "suite"]
assert len(suites) == 2 and suites[0]["event"] == "started" and suites[0]["test_count"] == len(expected)
assert suites[1]["event"] == "ok" and suites[1]["passed"] == len(expected)
assert all(suites[1][field] == 0 for field in ("failed", "ignored", "measured"))
assert suites[1]["filtered_out"] == len(names) - len(expected)
assert "skipping KVM" not in Path(output).read_text()
Path(output + ".verified.json").write_text(json.dumps(dict(expected=expected, executed=finished, counts=suites[1]), indent=2) + "\n")
PY
}

run_test_binary() {
    local label=$1 binary=$2 selected=$3 mode=$4
    local -a selection=()
    [[ -z $selected ]] || selection+=("$selected" --exact)
    [[ $mode != ignored ]] || selection+=(--ignored)
    if ! run_case "$label-list" "$binary" --list --format=json -Zunstable-options; then
        declare -gA CASE_DEPENDENCY
        CASE_DEPENDENCY[$label]="$label-list"
        return 1
    fi
    run_case "$label" env REVERIE_REQUIRE_KVM=1 "$binary" "${selection[@]}" \
        --test-threads=1 --format=json -Zunstable-options --show-output || return
    if grep -q 'skipping KVM' "$WORK_DIR/$label.stderr"; then
        comparison_failed
        return 1
    fi
    if ! verify_test_execution "$label" "$selected" "$mode"; then
        comparison_failed
        return 1
    fi
}

run_cargo_test() {
    local label=$1 target=$2 selected=$3 binary
    binary=$(test_binary_from_case prepare-runtime "$target") || return
    run_test_binary "$label" "$binary" "$selected" normal
}

measure_ptrace() {
    local fixture="$WORK_DIR/ptrace-chaos"
    local data="$WORK_DIR/ptrace-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record_runtime_failure ptrace B1 compile-ptrace-chaos \
            'real guest action and exact guest stdout' 'C fixture compilation failed'
        return
    fi
    if run_case ptrace-b1 "$TARGET_DIR/$PROFILE/chaos" --runner ptrace \
        --no-interrupt --no-host-envs "$fixture" "$data" &&
        cmp -s "$WORK_DIR/ptrace-b1.stdout" "$data" &&
        grep -Eq ', 1\) = 1$' "$WORK_DIR/ptrace-b1.stderr"; then
        record ptrace B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + read-length replacement trace' \
            'Chaos limited guest reads to one byte while preserving output; this witness is single-process and does not claim library-call interception; no backend fallback was used'
    else
        record_runtime_failure ptrace B1 ptrace-b1 \
            'exit status + exact guest stdout + read-length replacement trace' \
            'Chaos action was not proved'
        return
    fi

    local repeat counter1='' counter2='' ok=1 i
    for ((i = 1; i <= REPEATS; i++)); do
        repeat="ptrace-counter1-$i"
        run_case "$repeat" "$TARGET_DIR/$PROFILE/counter1" \
            --no-host-envs -- /bin/echo ptrace-counter1 || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/$repeat.stdout" "ptrace-counter1" || { ok=0; comparison_failed; }
        local value
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/$repeat.stderr" | tail -1)
        [[ -n $value ]] || { ok=0; comparison_failed; }
        [[ -z $counter1 || $counter1 == "$value" ]] || { ok=0; comparison_failed; }
        counter1=$value

        repeat="ptrace-counter2-$i"
        run_case "$repeat" "$TARGET_DIR/$PROFILE/counter2" \
            --no-host-envs -- /bin/sh -c '/bin/true & child=$!; wait "$child"' || { ok=0; comparison_failed; }
        [[ ! -s $WORK_DIR/$repeat.stdout ]] || { ok=0; comparison_failed; }
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/$repeat.stderr" | tail -1)
        [[ $value == *' 2 2' ]] || { ok=0; comparison_failed; }
        [[ -z $counter2 || $counter2 == "$value" ]] || { ok=0; comparison_failed; }
        counter2=$value

        repeat="ptrace-strace-$i"
        run_case "$repeat" "$TARGET_DIR/$PROFILE/strace" --runner ptrace \
            --trace write --no-host-envs /bin/echo ptrace-strace || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/$repeat.stdout" "ptrace-strace" || { ok=0; comparison_failed; }
        grep -Eq 'write\(1,.*\) = 14$' "$WORK_DIR/$repeat.stderr" || { ok=0; comparison_failed; }
    done
    if ((ok == 1)); then
        record ptrace B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable counter totals + process/thread totals + semantic write trace' \
            "counter1=$counter1; counter2=$counter2"
    else
        record_b15_failure ptrace ptrace- \
            'exit status + exact guest stdout + stable counter totals + process/thread totals + semantic write trace' \
            'one or more repeated exact-tool checks failed'
    fi
}

probe_kvm() {
    python3 -I - <<'PY'
import os
try:
    fd = os.open('/dev/kvm', os.O_RDWR | os.O_CLOEXEC)
except OSError as error:
    print(f'errno={error.errno} name={error.__class__.__name__} message={error}', flush=True)
    raise SystemExit(1)
else:
    os.close(fd)
    print('open(O_RDWR|O_CLOEXEC)=ok', flush=True)
PY
}

measure_kvm() {
    if ! probe_kvm >"$WORK_DIR/kvm-probe.stdout" 2>"$WORK_DIR/kvm-probe.stderr"; then
        local detail
        detail=$(<"$WORK_DIR/kvm-probe.stdout")
        detail+=" $(<"$WORK_DIR/kvm-probe.stderr")"
        record kvm B1 unmeasurable 0 missing missing missing not_measured \
            not_measured not_measured not_measured not_measured \
            'open /dev/kvm with O_RDWR|O_CLOEXEC' "$detail"
        record kvm B1.5 unmeasurable 0 missing missing missing missing \
            not_measured not_measured not_measured not_measured \
            'required KVM execution' 'B1 prerequisite was unmeasurable'
        return
    fi
    local fixture="$WORK_DIR/kvm-chaos" data="$WORK_DIR/kvm-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record_runtime_failure kvm B1 compile-kvm-chaos \
            'real KVM guest action' 'C fixture compilation failed'
        return
    fi
    if run_case kvm-b1 env REVERIE_REQUIRE_KVM=1 \
        "$TARGET_DIR/$PROFILE/chaos" --runner kvm --no-interrupt --no-host-envs \
        "$fixture" "$data" &&
        cmp -s "$WORK_DIR/kvm-b1.stdout" "$data" &&
        grep -Eq ', 1\) = 1$' "$WORK_DIR/kvm-b1.stderr"; then
        record kvm B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'required KVM execution + exact guest stdout + read arguments/return + read-length replacement trace' \
            'real guest ELF; Chaos limited reads to one byte; this witness is single-process and does not claim library-call interception; unsupported syscall paths remain backend errors rather than ptrace fallback'
    else
        record_runtime_failure kvm B1 kvm-b1 \
            'required KVM execution + exact guest stdout + read arguments/return + read-length replacement trace' \
            'KVM real-ELF Chaos action was not proved'
        return
    fi

    local i ok=1 counter1='' counter2='' value
    run_cargo_test kvm-b15-suite kvm_cli "" || { ok=0; comparison_failed; }
    for ((i = 1; i <= REPEATS; i++)); do
        run_case "kvm-counter1-$i" "$TARGET_DIR/$PROFILE/reverie-kvm-counter1" \
            /bin/echo kvm-counter1 || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/kvm-counter1-$i.stdout" "kvm-counter1" || { ok=0; comparison_failed; }
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/kvm-counter1-$i.stderr" | tail -1)
        [[ -n $value ]] || { ok=0; comparison_failed; }
        [[ -z $counter1 || $counter1 == "$value" ]] || { ok=0; comparison_failed; }
        counter1=$value

        run_case "kvm-counter2-$i" "$TARGET_DIR/$PROFILE/reverie-kvm-counter2" \
            /bin/echo kvm-counter2 || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/kvm-counter2-$i.stdout" "kvm-counter2" || { ok=0; comparison_failed; }
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/kvm-counter2-$i.stderr" | tail -1)
        [[ $value == *' 1 1' ]] || { ok=0; comparison_failed; }
        [[ -z $counter2 || $counter2 == "$value" ]] || { ok=0; comparison_failed; }
        counter2=$value

        run_case "kvm-strace-$i" "$TARGET_DIR/$PROFILE/strace" --runner kvm \
            --trace write --no-host-envs -- /bin/echo kvm-strace || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/kvm-strace-$i.stdout" "kvm-strace" || { ok=0; comparison_failed; }
        grep -Eq 'write\(1,.*\) = 11$' "$WORK_DIR/kvm-strace-$i.stderr" || { ok=0; comparison_failed; }
    done
    if ((ok == 1)); then
        record kvm B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable counter totals + semantic write trace + ptrace counter2 total on echo + process/thread totals on a process tree' \
            "counter1=$counter1; counter2=$counter2; process-tree syscall total is not compared; no canonical INFO comparison"
    else
        record_b15_failure kvm kvm- \
            'required KVM exact-tool and ptrace-comparison tests' \
            'one or more required non-skipping KVM tests failed'
    fi
}

dbt_paths() {
    DBT_CLIENT="$TARGET_DIR/release/reverie-dbt-native/libreverie_dbt_client.so"
    local helper
    helper=$(cargo_artifact prepare-dbt-rust reverie-dbt-dynamorio-path executable reverie-dbt) || return
    bind_artifact "$DBT_CLIENT" prepare-dbt || return
    run_case dbt-drrun-path "$helper" drrun || return
    run_case dbt-home-path "$helper" home || return
    DBT_DRRUN=$(<"$WORK_DIR/dbt-drrun-path.stdout")
    DBT_HOME=$(<"$WORK_DIR/dbt-home-path.stdout")
    bind_artifact "$DBT_DRRUN" prepare-dbt-rust || return
    export DBT_CLIENT DBT_DRRUN DBT_HOME
}

# StraceTool logs decoded arguments before tail injection and deliberately has
# no return value. The echo witness writes eleven bytes to stdout; the pointer
# must be non-null, while its address is not a deterministic comparison field.
verify_dbt_strace() {
    LC_ALL=C grep -Eq '^\[dbt strace pid [1-9][0-9]*\] write\(1, 0x[1-9a-f][0-9a-f]*, 11\) = \?$' "$1"
}

measure_dbt() {
    local fixture="$WORK_DIR/dbt-chaos" data="$WORK_DIR/dbt-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record_runtime_failure dbt B1 compile-dbt-chaos \
            'DynamoRIO client + real guest read replacement' \
            'required DBT artifact or C fixture is unavailable'
        return
    fi
    if run_case dbt-b1 env HERMIT_DBT_CHAOS=1 "$DBT_DRRUN" -quiet \
        -disable_rseq -stack_size 2M -c "$DBT_CLIENT" -- "$fixture" "$data" &&
        cmp -s "$WORK_DIR/dbt-b1.stdout" "$data" &&
        [[ $(grep -Ec 'chaos \[pid [0-9]+ n [0-9]+\] read\(.*\) = 1' \
            "$WORK_DIR/dbt-b1.stderr") -ge 10 ]]; then
        record dbt B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + observed read arguments/return + read-length replacement' \
            'real guest ELF reconstructed the file after 10+ one-byte reads; copied children do not run the Rust Tool'
    else
        record_runtime_failure dbt B1 dbt-b1 \
            'exit status + exact guest stdout + observed read arguments/return + read-length replacement' \
            'DBT Chaos action was not proved'
        return
    fi

    local i ok=1 counter1='' counter2='' problems=''
    for ((i = 1; i <= REPEATS; i++)); do
        # Coreutils closes stderr before process-exit callbacks run. Preserve
        # the same diagnostic descriptor used by the public DbtRunner.
        if ! run_case "dbt-counter1-$i" bash -c 'exec 198>&2; exec "$@"' dbt-counter1 \
            env HERMIT_DBT_COUNTER1_EXACT=1 "$DBT_DRRUN" \
            -quiet -disable_rseq -stack_size 2M -c "$DBT_CLIENT" \
            -diagnostic_fd 198 -- /bin/echo dbt-counter1; then
            ok=0
            comparison_failed
            problems+="counter1[$i] exit=$(case_status "dbt-counter1-$i"); "
        fi
        if ! exact_stdout "$WORK_DIR/dbt-counter1-$i.stdout" "dbt-counter1"; then
            ok=0
            comparison_failed
            problems+="counter1[$i] guest stdout differed; "
        fi
        local value
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/dbt-counter1-$i.stderr" | tail -1)
        if [[ -z $value ]]; then
            ok=0
            comparison_failed
            problems+="counter1[$i] missing exact summary; "
        elif [[ -n $counter1 && $counter1 != "$value" ]]; then
            ok=0
            comparison_failed
            problems+="counter1 total changed from $counter1 to $value; "
        fi
        counter1=$value

        if ! run_case "dbt-counter2-$i" env DYNAMORIO_HOME="$DBT_HOME" \
            REVERIE_DBT_CLIENT="$DBT_CLIENT" \
            "$TARGET_DIR/release/reverie-dbt-counter2-exact" -- /bin/echo dbt-counter2; then
            ok=0
            comparison_failed
            problems+="counter2[$i] exit=$(case_status "dbt-counter2-$i"); "
        fi
        if grep -q 'prototype stack overflow' "$WORK_DIR/dbt-counter2-$i.stderr"; then
            problems+="counter2[$i] DynamoRIO prototype stack overflow; "
        fi
        if ! exact_stdout "$WORK_DIR/dbt-counter2-$i.stdout" "dbt-counter2"; then
            ok=0
            comparison_failed
            problems+="counter2[$i] guest stdout differed; "
        fi
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/dbt-counter2-$i.stderr" | tail -1)
        if [[ $value != *' 1 1' ]]; then
            ok=0
            comparison_failed
            problems+="counter2[$i] missing 1-process/1-thread exact summary; "
        elif [[ -n $counter2 && $counter2 != "$value" ]]; then
            ok=0
            comparison_failed
            problems+="counter2 total changed from $counter2 to $value; "
        fi
        counter2=$value

        if ! run_case "dbt-strace-$i" env HERMIT_DBT_STRACE=1 "$DBT_DRRUN" -quiet \
            -disable_rseq -stack_size 2M -c "$DBT_CLIENT" -- /bin/echo dbt-strace; then
            ok=0
            comparison_failed
            problems+="strace[$i] exit=$(case_status "dbt-strace-$i"); "
        fi
        if ! exact_stdout "$WORK_DIR/dbt-strace-$i.stdout" "dbt-strace" ||
            ! verify_dbt_strace "$WORK_DIR/dbt-strace-$i.stderr"; then
            ok=0
            comparison_failed
            problems+="strace[$i] guest stdout or decoded stdout write event missing; "
        fi
    done
    if ((ok == 1)); then
        record dbt B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable exact counter totals + decoded write arguments (trace return unavailable)' \
            "counter1=$counter1; counter2=$counter2; prototype GlobalTool totals cover this single process; copied children do not run the Rust Tool"
    else
        record_b15_failure dbt dbt- \
            'exit status + exact guest stdout + stable exact counter totals + decoded write arguments (trace return unavailable)' \
            "$problems"
    fi
}

sabre_paths() {
    SABRE_RUNNER=$(cargo_artifact prepare-runtime reverie-sabre-strace executable reverie-sabre-strace) || return
    SABRE_PLUGIN=$(cargo_artifact prepare-runtime reverie_sabre_strace_plugin .so reverie-sabre-strace) || return
    SABRE_LOADER=$(native_build_artifact prepare-runtime experimental/reverie-sabre sabre-build-v4/sabre) || return
    export SABRE_RUNNER SABRE_PLUGIN SABRE_LOADER
}

run_sabre() {
    local label=$1 tool=$2
    shift 2
    run_case "$label" "$SABRE_RUNNER" --sabre "$SABRE_LOADER" \
        --plugin "$SABRE_PLUGIN" --tool "$tool" -- "$@"
}

measure_sabre() {
    local fixture="$WORK_DIR/sabre-chaos" data="$WORK_DIR/sabre-chaos.txt"
    printf 'CHAOS-ONE-BYTE-AT-A-TIME\n' >"$data"
    if ! compile_fixture reverie-dbt/tests/fixtures/chaos_read_file.c "$fixture"; then
        record_runtime_failure sabre B1 compile-sabre-chaos \
            'SaBRe loader/plugin + real guest action' 'required artifact or fixture is unavailable'
        return
    fi
    if run_case sabre-b1 "$SABRE_RUNNER" --sabre "$SABRE_LOADER" \
        --plugin "$SABRE_PLUGIN" --tool chaos --no-interrupt -- "$fixture" "$data" &&
        cmp -s "$WORK_DIR/sabre-b1.stdout" "$data" &&
        grep -Eq ', 1\) = 1$' "$WORK_DIR/sabre-b1.stderr"; then
        record sabre B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + read-length replacement trace' \
            'Chaos limited guest reads to one byte while preserving output; this witness is single-process and does not claim library-call interception; no ptrace fallback was used'
    else
        record_runtime_failure sabre B1 sabre-b1 \
            'exit status + exact guest stdout + read-length replacement trace' \
            'SaBRe Chaos action was not proved'
        return
    fi

    local i ok=1 counter1='' counter2=''
    for ((i = 1; i <= REPEATS; i++)); do
        run_sabre "sabre-counter1-$i" counter1-exact /bin/echo sabre-counter1 || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/sabre-counter1-$i.stdout" "sabre-counter1" || { ok=0; comparison_failed; }
        local value
        value=$(sed -n 's/.*counter1-global syscalls=\([0-9][0-9]*\).*/\1/p' \
            "$WORK_DIR/sabre-counter1-$i.stderr" | tail -1)
        [[ -n $value ]] || { ok=0; comparison_failed; }
        [[ -z $counter1 || $counter1 == "$value" ]] || { ok=0; comparison_failed; }
        counter1=$value

        run_sabre "sabre-counter2-$i" counter2-exact /bin/sh -c \
            '/bin/true & child=$!; wait "$child"' || { ok=0; comparison_failed; }
        [[ ! -s $WORK_DIR/sabre-counter2-$i.stdout ]] || { ok=0; comparison_failed; }
        value=$(sed -n \
            -e 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            -e 's/.*counter2-global syscalls=\([0-9][0-9]*\) processes=\([0-9][0-9]*\) threads=\([0-9][0-9]*\).*/\1 \2 \3/p' \
            "$WORK_DIR/sabre-counter2-$i.stderr" | tail -1)
        [[ $value == *' 2 2' ]] || { ok=0; comparison_failed; }
        [[ -z $counter2 || $counter2 == "$value" ]] || { ok=0; comparison_failed; }
        counter2=$value

        run_sabre "sabre-strace-$i" strace-minimal /bin/echo sabre-strace || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/sabre-strace-$i.stdout" "sabre-strace" || { ok=0; comparison_failed; }
        grep -Eq 'write\(1,.*, 13\) = \?$' "$WORK_DIR/sabre-strace-$i.stderr" || { ok=0; comparison_failed; }
    done
    if ((ok == 1)); then
        record sabre B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable exact counter totals + process/thread totals + semantic write arguments in trace adapter' \
            "counter1=$counter1; counter2=$counter2; trace adapter reports return value as unknown"
    else
        record_b15_failure sabre sabre- \
            'exit status + exact guest stdout + stable exact counter totals + process/thread totals + semantic write trace' \
            'one or more repeated exact-tool checks failed'
    fi
}

measure_liteinst() {
    local ok=1 i tree_detail preload counter1='' counter2='' value
    if run_cargo_test liteinst-b1 liteinst exact_chaos_tool_limits_reads_after_skip; then
        record liteinst B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + asserted read-length replacement' \
            'exact Chaos integration test; B1 witness is single-process and does not claim library-call interception; no ptrace fallback was used'
    else
        record_runtime_failure liteinst B1 liteinst-b1 \
            'exit status + exact guest stdout + asserted read-length replacement' \
            'LiteInst Chaos action was not proved'
        return
    fi
    if ! preload=$(cargo_artifact prepare-runtime reverie_examples .so reverie-examples); then
        record liteinst B1.5 unmeasurable 0 missing missing missing missing \
            not_measured not_measured not_measured not_measured \
            'repeated exact counter1/counter2/strace execution' \
            'the required tool preload artifact is missing'
        return
    fi
    for ((i = 1; i <= REPEATS; i++)); do
        run_case "liteinst-counter1-$i" env \
            REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
            "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool counter1 \
            --preload "$preload" -- /bin/echo liteinst-counter1 || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/liteinst-counter1-$i.stdout" "liteinst-counter1" || { ok=0; comparison_failed; }
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\)$/\1/p' \
            "$WORK_DIR/liteinst-counter1-$i.stderr" | tail -1)
        [[ -n $value ]] || { ok=0; comparison_failed; }
        [[ -z $counter1 || $counter1 == "$value" ]] || { ok=0; comparison_failed; }
        counter1=$value

        run_case "liteinst-counter2-$i" env \
            REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
            "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool counter2 \
            --preload "$preload" -- /bin/echo liteinst-counter2 || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/liteinst-counter2-$i.stdout" "liteinst-counter2" || { ok=0; comparison_failed; }
        value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/liteinst-counter2-$i.stderr" | tail -1)
        [[ $value == *' 1 1' ]] || { ok=0; comparison_failed; }
        [[ -z $counter2 || $counter2 == "$value" ]] || { ok=0; comparison_failed; }
        counter2=$value

        run_case "liteinst-strace-$i" env \
            REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
            "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool strace \
            --preload "$preload" --trace write -- /bin/echo liteinst-strace || { ok=0; comparison_failed; }
        exact_stdout "$WORK_DIR/liteinst-strace-$i.stdout" "liteinst-strace" || { ok=0; comparison_failed; }
        grep -Eq 'write\(1,.*\) = 16$' "$WORK_DIR/liteinst-strace-$i.stderr" || { ok=0; comparison_failed; }
    done
    if run_case liteinst-process-tree timeout --signal=TERM --kill-after=2s 10s \
        env REVERIE_LITEINST_STRADDLER_STALENESS_TICKS=20000 \
        "$TARGET_DIR/$PROFILE/reverie-liteinst-examples" --tool counter2 \
        --preload "$preload" -- \
        /bin/sh -c '/bin/true & child=$!; wait "$child"'; then
        local tree_value
        tree_value=$(sed -n 's/.*Total system calls in process tree: \([0-9][0-9]*\), from \([0-9][0-9]*\) processes, \([0-9][0-9]*\) thread(s).*/\1 \2 \3/p' \
            "$WORK_DIR/liteinst-process-tree.stderr" | tail -1)
        if [[ $tree_value == *' 2 2' ]]; then
            tree_detail="process-tree counter2 completed with $tree_value"
        else
            ok=0
            comparison_failed
            tree_detail='process-tree counter2 completed without the expected 2-process/2-thread summary'
        fi
    elif [[ $(case_status liteinst-process-tree) == 124 ]]; then
        ok=0
        tree_detail='process-tree counter2 timed out after 10 seconds'
    else
        ok=0
        tree_detail="process-tree counter2 exited $(case_status liteinst-process-tree)"
    fi
    if ((ok == 1)); then
        record liteinst B1.5 pass "$REPEATS" compared compared compared compared \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + stable exact counter totals + semantic write trace' \
            "counter1=$counter1; counter2=$counter2; $tree_detail"
    else
        record_b15_failure liteinst liteinst- \
            'repeated exact counter1/counter2/strace integration tests' \
            "one or more exact-tool tests failed; $tree_detail"
    fi
}

e9patch_paths() {
    E9TOOL=$(native_build_artifact prepare-runtime reverie-e9patch e9patch-build/e9tool) || return
    E9PATCH=$(native_build_artifact prepare-runtime reverie-e9patch e9patch-build/e9patch) || return
    export E9TOOL E9PATCH
}

run_e9patch_test() {
    local label=$1 test_binary=$2 test_name=$3
    REVERIE_E9TOOL="$E9TOOL" REVERIE_E9PATCH_BACKEND="$E9PATCH" \
        run_test_binary "$label" "$test_binary" "$test_name" ignored
}

measure_e9patch() {
    local E9_EXAMPLES_TEST
    if ! e9patch_paths; then
        record e9patch B1 unmeasurable 0 missing missing missing not_applicable \
            not_measured not_measured not_measured not_measured \
            'e9tool/e9patch pair + direct Tool action' 'required built pair is unavailable'
        return
    fi
    if ! E9_EXAMPLES_TEST=$(test_binary_from_case prepare-runtime e9patch_direct); then
        record e9patch B1 unmeasurable 0 missing missing missing not_applicable \
            not_measured not_measured not_measured not_measured \
            'e9patch direct Tool tests' 'required test executables could not be built'
        return
    fi
    if run_e9patch_test e9patch-b1 "$E9_EXAMPLES_TEST" \
        production_strace_observes_filtered_rewritten_write; then
        record e9patch B1 pass 1 compared compared compared not_applicable \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + observed write arguments/result + injected write action' \
            'real root guest ELF executed a rewritten write through the direct strace Tool; shared-library sites and process creation are outside this path; no ptrace fallback was used'
    else
        record_runtime_failure e9patch B1 e9patch-b1 \
            'exit status + exact guest stdout + observed write arguments/result + injected write action' \
            'e9patch direct strace action was not proved'
        return
    fi
    local i ok=1 test
    for ((i = 1; i <= REPEATS; i++)); do
        for test in production_counter1_reports_rewritten_syscall_total \
            production_counter2_reports_exit_lifecycle_totals \
            production_strace_observes_filtered_rewritten_write; do
            run_e9patch_test "e9patch-$test-$i" "$E9_EXAMPLES_TEST" "$test" || { ok=0; comparison_failed; }
        done
    done
    if ((ok == 1)); then
        record e9patch B1.5 pass "$REPEATS" compared compared compared limited \
            not_measured not_measured not_measured not_measured \
            'exit status + exact guest stdout + exact counter totals + exit lifecycle totals + semantic write trace' \
            'single-process direct AOT evidence; process creation is outside this backend path'
    else
        record_b15_failure e9patch e9patch- \
            'repeated exact counter1/counter2/strace integration tests' \
            'one or more production exact-tool tests failed'
    fi
}

rank() {
    case "$1" in
        none) printf 0 ;;
        B0) printf 1 ;;
        B1) printf 2 ;;
        B1.5) printf 3 ;;
        *) return 1 ;;
    esac
}

selected() {
    local candidate=$1 backend
    for backend in "${BACKEND_LIST[@]}"; do
        [[ $backend == "$candidate" ]] && return 0
    done
    return 1
}

derive_maximum() {
    local backend=$1 maximum=none
    [[ ${LEVEL_OUTCOME["$backend:B0"]:-missing} == pass ]] && maximum=B0
    [[ $maximum == B0 && ${LEVEL_OUTCOME["$backend:B1"]:-missing} == pass ]] && maximum=B1
    [[ $maximum == B1 && ${LEVEL_OUTCOME["$backend:B1.5"]:-missing} == pass ]] && maximum=B1.5
    MAXIMUM_LEVEL[$backend]=$maximum
}

BUILD_CONTEXT_SHA256=$(build_context "$WORK_DIR/build-context.json") || exit 2
readonly BUILD_CONTEXT_SHA256
run_case metadata cargo metadata --locked --offline --no-deps --format-version=1 || exit 2

for backend_package in \
    ptrace:reverie-ptrace \
    kvm:reverie-kvm \
    dbt:reverie-dbt \
    sabre:reverie-sabre-strace \
    liteinst:reverie-liteinst \
    e9patch:reverie-e9patch; do
    selected "${backend_package%%:*}" && \
        measure_b0 "${backend_package%%:*}" "${backend_package#*:}"
done

# Runtime binaries are deliberately built after the release-build
# rows. A failed preparation cannot retroactively turn a release-build failure
# into a pass.
PREPARED=1
PREPARED_OUTCOME=pass
if [[ $SKIP_PREPARE != 1 ]]; then
    runtime_packages=()
    runtime_targets=(--bins --lib)
    if selected ptrace || selected kvm || selected liteinst || selected e9patch; then
        runtime_packages+=(-p reverie-examples)
        for backend_target in kvm:kvm_cli liteinst:liteinst e9patch:e9patch_direct; do
            if selected "${backend_target%%:*}"; then
                runtime_targets+=(--test "${backend_target#*:}")
            fi
        done
    fi
    if selected sabre; then
        runtime_packages+=(-p reverie-sabre-strace)
    fi
    if ((${#runtime_packages[@]} != 0)); then
        # Resolve all debug runtime packages and integration tests together.
        # Separate builds can overwrite shared unversioned dependency rlibs
        # before the original producing command is bound.
        run_case prepare-runtime cargo build --locked --message-format=json \
            "${runtime_packages[@]}" "${runtime_targets[@]}" || PREPARED=0
    fi
    if selected dbt; then
        # DynamoRIO's client stack cannot safely host debug Rust frames. Match
        # its production configuration for both the runtime and native client.
        run_case prepare-dbt-rust cargo build --locked --release --message-format=json -p reverie-dbt || PREPARED=0
        run_case prepare-dbt env PROFILE=release \
            "$ROOT_DIR/reverie-dbt/scripts/build-client.sh" || PREPARED=0
    fi
    if ((PREPARED == 1)); then
        if ((${#runtime_packages[@]} != 0)); then
            bind_cargo_outputs prepare-runtime || exit 2
        fi
        if selected dbt; then
            bind_cargo_outputs prepare-dbt-rust || exit 2
            dbt_paths || exit 2
        fi
        if selected sabre; then
            sabre_paths || exit 2
        fi
        for backend_target in kvm:kvm_cli liteinst:liteinst e9patch:e9patch_direct; do
            if selected "${backend_target%%:*}"; then
                test_binary_from_case prepare-runtime "${backend_target#*:}" >"$WORK_DIR/${backend_target#*:}.binary" || exit 2
            fi
        done
        if selected liteinst; then
            check_test_preload prepare-runtime liteinst || exit 2
        fi
        if selected e9patch; then
            check_test_preload prepare-runtime e9patch_direct || exit 2
            e9patch_paths || exit 2
        fi
        retain_runtime_dependencies || exit 2
        verify_bound_artifacts || exit 2
        ARTIFACTS_SEALED=1
    fi
else
    PREPARED=0
    PREPARED_OUTCOME=unmeasurable
fi
if ((PREPARED == 0)) && [[ $SKIP_PREPARE != 1 ]]; then
    if ! any_execution_failed prepare- && any_infrastructure_denied prepare-; then
        PREPARED_OUTCOME=unmeasurable
    else
        PREPARED_OUTCOME=fail
    fi
    printf 'WARN: one or more runtime-artifact builds failed; affected rows will be fail or unmeasurable\n' >&2
fi

if ((PREPARED == 1)); then
    selected ptrace && measure_ptrace
    selected kvm && measure_kvm
    selected dbt && measure_dbt
    selected sabre && measure_sabre
    selected liteinst && measure_liteinst
    selected e9patch && measure_e9patch
else
    for backend in "${BACKEND_LIST[@]}"; do
        for level in B1 B1.5; do
            record "$backend" "$level" "$PREPARED_OUTCOME" 0 missing missing missing missing \
                not_measured not_measured not_measured not_measured \
                'current-run runtime preparation' 'required fresh artifact preparation failed or was explicitly omitted; no runtime witness awarded'
        done
    done
fi

printf '\nMaximum defensible maturity on this run\n'
printf '%-10s %-10s %-10s\n' backend maximum minimum
overall=0
for backend in "${BACKEND_LIST[@]}"; do
    derive_maximum "$backend"
    printf '%-10s %-10s %-10s\n' "$backend" "${MAXIMUM_LEVEL[$backend]}" "${MINIMUM_LEVEL[$backend]}"
    # A lower declared maturity minimum describes partial capability; it never
    # excuses a failed or unavailable measurement that this gate actually ran.
    # Every selected B0/B1/B1.5 row must pass before the driver may record PASS.
    for level in B0 B1 B1.5; do
        case ${LEVEL_OUTCOME["$backend:$level"]:-missing} in
            pass) ;;
            fail) overall=1 ;;
            *) [[ $overall == 1 ]] || overall=2 ;;
        esac
    done
    if (( $(rank "${MAXIMUM_LEVEL[$backend]}") < $(rank "${MINIMUM_LEVEL[$backend]}") )); then
        backend_result=2
        for level in B0 B1 B1.5; do
            (( $(rank "$level") <= $(rank "${MINIMUM_LEVEL[$backend]}") )) || break
            outcome=${LEVEL_OUTCOME["$backend:$level"]:-missing}
            if [[ $outcome == fail ]]; then
                backend_result=1
                break
            fi
        done
        if ((backend_result == 1)); then
            overall=1
        elif ((overall == 0)); then
            overall=2
        fi
    fi
done

if ((PARTIAL_SELECTION == 1 || SKIP_PREPARE == 1)); then
    [[ $overall == 1 ]] || overall=2
fi
if [[ $PREPARED_OUTCOME == fail ]]; then
    overall=1
elif [[ $PREPARED_OUTCOME == unmeasurable && $overall == 0 ]]; then
    overall=2
fi

printf '\nB2 and above: not measured. This validate does not implement or substitute for the shared canonical Hermit predicate.\n'
printf 'Report: %s\n' "$REPORT"
verify_source final
verify_build_context final
if ((ARTIFACTS_SEALED == 1)); then
    verify_bound_artifacts || exit 2
fi
capture_build_outputs final || exit 2
python3 -I - "$WORK_DIR" "$overall" "$SOURCE_TREE_SHA256" "$BUILD_CONTEXT_SHA256" <<'PY' || exit 2
import hashlib
import json
import sys
from pathlib import Path

work, status, source, context = Path(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4]
files = {}
for path in work.iterdir():
    if path.is_file() and path.name != "result.json":
        with path.open("rb") as stream:
            files[path.name] = dict(sha256=hashlib.file_digest(stream, "sha256").hexdigest(), bytes=path.stat().st_size)
assert "report.tsv" in files and "source.json" in files and "build-context.json" in files
record = dict(exit_status=status, source_manifest_sha256=source, build_context_sha256=context,
              retained_files=files, canonical_hermit_evidence=False,
              internal_test_generated_guest_bytes_exported=False)
(work / "result.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
PY
exit "$overall"
