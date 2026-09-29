# Containment fixture sudo dependency: audit (2026-09-25, head b6b8f7a16a8a)

Question: can `_disposable_btrfs_verify_root`
(`fbcode/testinfra/tfa/tests/test_production_containment.py:204-457`) drop
`sudo -n`, or be replaced by an equally strong unprivileged bounded-storage test,
without skipping or weakening its cleanup/recovery assertions?

Verdict: no, not from inside TFA. This is an infra-owner blocker, proven below.
No code change was made. A change that "removes" sudo would either test a
different backend than production or be refused by production as UNBOUNDED.

## 1. The fixture's sudo is not the real dependency. Production's is.

The fixture uses sudo for `mount -o loop`, `btrfs quota enable`, `chown` and
`umount`. Dropping those four calls wouldn't help, because the code under test,
`fbcode/testinfra/tfa/resources/bounded-run-space`, itself requires passwordless
sudo:

- `:96` `have_sudo() { sudo -n true; }`
- `:192` UNBOUNDED reason "passwordless sudo unavailable; subvolume create/delete both need it", then `return 10`
- `:202` `sudo -n btrfs subvolume create`
- `:216` `sudo -n btrfs qgroup limit`, followed by a readback check (`:219-225`)
- `:99`, `:105` `sudo -n btrfs subvolume show` / `qgroup show`
- `:126`, `:229` `sudo -n btrfs subvolume delete`

`hermit.py` `_prepare_verify_space` (562-619) refuses to launch when
`bounded-run-space` exits 10. So without sudo, every decorated containment test
fails in production code, not in the fixture.

Btrfs qgroup ioctls (`BTRFS_IOC_QUOTA_CTL`, `BTRFS_IOC_QGROUP_LIMIT`) check
`CAP_SYS_ADMIN` against the init user namespace. A user namespace grants no
such capability, so an unprivileged equivalent does not exist. The
`bounded-run-space` header (`:14`) records why cgroup `io.max` or tmpfs `size=`
are not substitutes: io.max limits bandwidth, not bytes at rest, and a tmpfs
limit is not the Btrfs qgroup that production enforces and reads back.

## 2. CI offers no lane that satisfies both constraints

The tests need both of these at once:

- (a) non-root execution. `process_supervisor.py:137` has
  `if os.getuid() == 0 or os.getuid() != os.geteuid(): os._exit(125)`, and the
  test asserts euid != 0 (`test_production_containment.py:3318`).
- (b) passwordless `sudo -n` for that non-root user.

The Sandcastle/Citadel lanes:

- Default lane: `SandcastleCitadelBuckExecutor.php:301` `getShellUser()` returns
  `getUnprivilegedUser()` (twsvcscm). twsvcscm has no passwordless sudo, so (b)
  fails. This is the observed CI failure: `sudo -n mount` exits non-zero and
  `_fixture_command` raises AssertionError.
- `uses_sudo` / `sudo_python_unittest` lane: routes to a `-sudo` job with
  `run_as_root`, and `getShellUser()` returns `'root'`. That breaks (a), so the
  supervisor exits 125 and the euid assertion fails.
- sudotest `sudo -E --user X`: X has no sudo either, so (b) fails.

## 3. The only route without host sudo is not viable now

An antlir2 `vm.python_test`
(`fbcode/antlir/antlir2/antlir2_vm/bzl/test.bzl` ~347) runs the test as root
inside the guest, with no `run_as_user`, so (a) fails. Making it work would need
all of the following, none of which exist in the tree:

- a guest user plus sudoers
- cgroup v2 delegation inside the guest
- a Hermit qualification with guest vPMU (`-cpu host -enable-kvm`). Hermit's
  guide warns that PMU/CPUID behaviour in VMs is unqualified.
- KVM on the CI worker

There is no Hermit or testinfra VM-test precedent. Verifying it locally would
need host capability probes that are forbidden for this work.

## 4. Cleanup/recovery on the CI denial path is already correct

I traced the denial path by reading `_disposable_btrfs_verify_root`.

When mount is refused, `mounted` stays False and `finally` does the following:

1. Restores `TFA_TEST_VERIFY_SPACE_ROOT`.
2. Writes the `cleanup` receipt (0600, fsynced).
3. Skips umount.
4. Runs `_require_fixture_root` (identity check), then `shutil.rmtree(root)`,
   then `_clear_fixture_recovery`.

A cleanup error is attached as a note to the body error, so neither error
masks the other. Nothing leaks and no assertion needs weakening. The failure
is reported loudly as a sudo denial, which is the honest signal.

## Owner actions that would unblock this (none are TFA-owned)

1. Sandcastle/Citadel: a lane that runs as a non-root user with scoped
   passwordless sudo for `mount`, `umount`, `btrfs` and `chown`.
2. Or antlir2: a `run_as_user` option for `vm.python_test` with delegated
   cgroups, plus a Hermit PMU-in-VM qualification.
