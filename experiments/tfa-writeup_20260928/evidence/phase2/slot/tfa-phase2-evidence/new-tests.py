def _load_payload_wrapper() -> types.ModuleType:
    path = Path(process_module.__file__).with_name("payload_wrapper.py")
    spec = importlib.util.spec_from_file_location("tfa_payload_wrapper_test", path)
    if spec is None or spec.loader is None:
        raise AssertionError("payload wrapper source cannot be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _open_descriptor_count() -> int:
    return len(os.listdir("/proc/self/fd"))


class _FakeClock:
    def __init__(self, now: float) -> None:
        self.now = now

    def monotonic(self) -> float:
        return self.now

    def time_ns(self) -> int:
        return time.time_ns()

    def sleep(self, seconds: float) -> None:
        self.now += seconds


class CodexIdentityHardeningTest(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        os.chmod(self.root, 0o755)
        for name, value in (
            ("_TRUSTED_OWNER_UID", os.getuid()),
            ("_TRUSTED_ROOT", self.root),
        ):
            patcher = patch.object(process_module, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def _trusted_file(
        self, relative: str, content: bytes = b"codex\n", mode: int = 0o755
    ) -> Path:
        path = self.root / relative
        path.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
        path.write_bytes(content)
        os.chmod(path, mode)
        return path

    def test_trusted_record_accepts_owner_only_file_as_control(self) -> None:
        path = self._trusted_file("bin/codex")
        record = process_module._root_owned_regular_record(path, "executable")
        self.assertEqual(record["path"], str(path))
        self.assertEqual(record["sha256"], hashlib.sha256(b"codex\n").hexdigest())
        self.assertEqual(record["owner_uid"], os.getuid())
        self.assertEqual(record["link_count"], 1)

    def test_trusted_record_rejects_foreign_owner(self) -> None:
        path = self._trusted_file("bin/codex")
        with (
            patch.object(process_module, "_TRUSTED_OWNER_UID", os.getuid() + 1),
            self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"),
        ):
            process_module._root_owned_regular_record(path, "executable")

    def test_trusted_record_rejects_additional_hard_link(self) -> None:
        path = self._trusted_file("bin/codex")
        alias = path.with_name("alias")
        os.link(path, alias)
        for candidate in (path, alias):
            with self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"):
                process_module._root_owned_regular_record(candidate, "executable")

    def test_trusted_record_rejects_group_or_world_writable_mode(self) -> None:
        for mode in (0o775, 0o757, 0o664, 0o646):
            with self.subTest(mode=oct(mode)):
                path = self._trusted_file(f"bin/codex-{mode:o}", mode=mode)
                with self.assertRaisesRegex(
                    ProcessError, "ownership or mode is unsafe"
                ):
                    process_module._root_owned_regular_record(path, "executable")

    def test_trusted_record_rejects_symlink(self) -> None:
        target = self._trusted_file("bin/codex")
        link = target.with_name("link")
        link.symlink_to(target)
        with self.assertRaisesRegex(
            ProcessError, "Codex executable (is unavailable|ownership or mode)"
        ):
            process_module._root_owned_regular_record(link, "executable")

    def test_trusted_record_rejects_writable_parent_directory(self) -> None:
        path = self._trusted_file("shared/bin/codex")
        process_module._root_owned_regular_record(path, "executable")
        for mode in (0o777, 0o775, 0o757):
            with self.subTest(mode=oct(mode)):
                os.chmod(self.root / "shared", mode)
                with self.assertRaisesRegex(
                    ProcessError, "executable directory chain is unsafe"
                ):
                    process_module._root_owned_regular_record(path, "executable")
        os.chmod(self.root / "shared", 0o755)

    def test_trusted_record_rejects_symlinked_parent_directory(self) -> None:
        self._trusted_file("real/codex")
        (self.root / "alias").symlink_to(self.root / "real")
        with self.assertRaisesRegex(
            ProcessError, "executable directory chain is unsafe"
        ):
            process_module._root_owned_regular_record(
                self.root / "alias" / "codex", "executable"
            )

    def test_directory_chain_rejects_foreign_owner_and_escapes(self) -> None:
        directory = self.root / "bin"
        directory.mkdir(mode=0o755)
        process_module._require_trusted_directory_chain(directory, "executable")
        with (
            patch.object(process_module, "_TRUSTED_OWNER_UID", os.getuid() + 1),
            self.assertRaisesRegex(ProcessError, "directory chain is unsafe"),
        ):
            process_module._require_trusted_directory_chain(directory, "executable")
        for candidate in (
            Path("bin"),
            self.root / "bin" / ".." / "bin",
            self.root.parent,
        ):
            with (
                self.subTest(candidate=str(candidate)),
                self.assertRaisesRegex(ProcessError, "directory chain is unsafe"),
            ):
                process_module._require_trusted_directory_chain(
                    candidate, "executable"
                )
        with self.assertRaisesRegex(ProcessError, "directory chain is unavailable"):
            process_module._require_trusted_directory_chain(
                self.root / "missing", "executable"
            )

    def test_release_manifest_requires_exact_channel_and_identity(self) -> None:
        stable = self._trusted_file("cli/codex.real", b"stable\n")
        latest = self._trusted_file("cli/codex_latest.real", b"latest\n")
        manifests = {
            "stable": (stable, hashlib.sha256(b"stable\n").hexdigest()),
            "latest": (latest, hashlib.sha256(b"latest\n").hexdigest()),
        }
        with patch.object(process_module, "_CODEX_RELEASE_MANIFESTS", manifests):
            for environment, channel, path in (
                ({}, "stable", stable),
                ({"META_CODEX_RELEASE": ""}, "stable", stable),
                ({"META_CODEX_RELEASE": "latest"}, "latest", latest),
            ):
                with self.subTest(environment=environment):
                    selected, record = process_module._codex_release_identity(
                        environment
                    )
                    self.assertEqual(selected, channel)
                    self.assertEqual(record["path"], str(path))
            for requested in (
                "stable",
                "LATEST",
                " latest",
                "latest\n",
                "0.157.0",
            ):
                with (
                    self.subTest(requested=requested),
                    self.assertRaisesRegex(
                        ProcessError, "release channel is not reviewed"
                    ),
                ):
                    process_module._codex_release_identity(
                        {"META_CODEX_RELEASE": requested}
                    )

    def test_release_manifest_rejects_digest_path_mode_and_link_drift(self) -> None:
        stable = self._trusted_file("cli/codex.real", b"stable\n")
        digest = hashlib.sha256(b"stable\n").hexdigest()
        with patch.object(
            process_module, "_CODEX_RELEASE_MANIFESTS", {"stable": (stable, "0" * 64)}
        ):
            with self.assertRaisesRegex(ProcessError, "manifest identity differs"):
                process_module._codex_release_identity({})
        with patch.object(
            process_module, "_CODEX_RELEASE_MANIFESTS", {"stable": (stable, digest)}
        ):
            process_module._codex_release_identity({})
            with patch.object(
                process_module,
                "regular_file_record",
                return_value={
                    "kind": "file",
                    "mode": 0o755,
                    "path": str(stable.with_name("other")),
                    "sha256": digest,
                    "size": 7,
                },
            ):
                with self.assertRaisesRegex(ProcessError, "manifest identity differs"):
                    process_module._codex_release_identity({})
            os.chmod(stable, 0o664)
            with self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"):
                process_module._codex_release_identity({})
            os.chmod(stable, 0o755)
            os.link(stable, stable.with_name("codex.real.alias"))
            with self.assertRaisesRegex(ProcessError, "ownership or mode is unsafe"):
                process_module._codex_release_identity({})

    def test_entrypoint_requires_trusted_symlink(self) -> None:
        target = self._trusted_file("cli/codex")
        entrypoint = self.root / "bin" / "codex"
        entrypoint.parent.mkdir(mode=0o755)
        entrypoint.symlink_to(target)
        with patch.object(process_module, "_CODEX_EXECUTABLE", str(entrypoint)):
            identity = process_module._codex_entrypoint_identity()
            self.assertEqual(identity["resolved_path"], str(target))
            with (
                patch.object(process_module, "_TRUSTED_OWNER_UID", os.getuid() + 1),
                self.assertRaisesRegex(ProcessError, "ownership or type is unsafe"),
            ):
                process_module._codex_entrypoint_identity()
            os.chmod(entrypoint.parent, 0o777)
            with self.assertRaisesRegex(
                ProcessError, "entrypoint directory chain is unsafe"
            ):
                process_module._codex_entrypoint_identity()
            os.chmod(entrypoint.parent, 0o755)
            entrypoint.unlink()
            entrypoint.write_bytes(b"not a symlink\n")
            with self.assertRaisesRegex(ProcessError, "ownership or type is unsafe"):
                process_module._codex_entrypoint_identity()

    def test_launcher_descriptor_rejects_changed_identity(self) -> None:
        path = self._trusted_file("cli/codex")
        record = process_module._root_owned_regular_record(path, "executable")
        descriptor, launch_path = process_module._open_verified_codex_launcher(record)
        os.close(descriptor)
        self.assertEqual(launch_path, str(path))
        before = _open_descriptor_count()
        os.chmod(path, 0o700)
        with self.assertRaisesRegex(ProcessError, "changed before launch"):
            process_module._open_verified_codex_launcher(record)
        os.chmod(path, 0o755)
        with path.open("ab") as stream:
            stream.write(b"grown\n")
        with self.assertRaisesRegex(ProcessError, "changed before launch"):
            process_module._open_verified_codex_launcher(record)
        replacement = path.with_name("replacement")
        replacement.write_bytes(b"codex\n")
        os.chmod(replacement, 0o755)
        os.replace(replacement, path)
        with self.assertRaisesRegex(ProcessError, "changed before launch"):
            process_module._open_verified_codex_launcher(record)
        link = path.with_name("link")
        link.symlink_to(path)
        with self.assertRaisesRegex(ProcessError, "cannot be pinned"):
            process_module._open_verified_codex_launcher({**record, "path": str(link)})
        self.assertEqual(_open_descriptor_count(), before)

    def _preflight_with_entry(
        self, value: tuple[str, str | None, str | None]
    ) -> tuple[dict[str, object], ProcessError | None]:
        executable = {
            "kind": "file",
            "path": "/usr/local/bin/codex_cli/codex",
            "sha256": "0" * 64,
        }
        manifest = {"path": "/usr/local/bin/codex_cli/codex.real", "sha256": "1" * 64}
        key = ("codex-cli 9.9.9", "0" * 64, "1" * 64, "stable")
        version = MagicMock(returncode=0, stdout="codex-cli 9.9.9\n", stderr="")
        output = self.root / f"preflight-{uuid.uuid4().hex}"
        error: ProcessError | None = None
        with (
            patch.dict(process_module._CODEX_COMPATIBILITY, {key: value}),
            patch.object(TrustedSubprocessWithList, "run", return_value=version),
            patch(
                "testinfra.tfa.process._codex_entrypoint_identity",
                return_value={"resolved_path": executable["path"]},
            ),
            patch(
                "testinfra.tfa.process._root_owned_regular_record",
                return_value=executable,
            ),
            patch(
                "testinfra.tfa.process._codex_release_identity",
                return_value=("stable", manifest),
            ),
        ):
            try:
                process_module._codex_environment_preflight(self.root, output, {})
            except ProcessError as raised:
                error = raised
        self.assertNotIn(key, process_module._CODEX_COMPATIBILITY)
        return json.loads((output / "identity.json").read_text()), error

    def test_eligible_entry_requires_hash_bound_payload(self) -> None:
        for payload in (None, "", "A" * 64, "0" * 63, "g" * 64):
            with self.subTest(payload=payload):
                identity, error = self._preflight_with_entry(
                    ("eligible", None, payload)
                )
                self.assertIsNotNone(error)
                self.assertEqual(identity["state"], "blocked")
                self.assertEqual(
                    identity["blocker"],
                    "eligible Codex compatibility entry lacks a hash-bound payload",
                )
                self.assertIsNone(identity["payload_sha256"])
        identity, error = self._preflight_with_entry(("eligible", None, "a" * 64))
        self.assertIsNone(error)
        self.assertEqual(identity["state"], "eligible")
        self.assertEqual(identity["payload_sha256"], "a" * 64)

    def test_production_compatibility_entries_stay_blocked_and_unbound(self) -> None:
        self.assertEqual(len(process_module._CODEX_COMPATIBILITY), 2)
        for key, value in process_module._CODEX_COMPATIBILITY.items():
            with self.subTest(key=key):
                self.assertEqual(
                    value,
                    ("blocked", process_module._CODEX_SOCKET_ISOLATION_BLOCKER, None),
                )
                _assert_socket_isolation_blocker_text(self, value[1])

    def test_preflight_requires_banner_of_requested_channel(self) -> None:
        executable = {"path": "/usr/local/bin/codex_cli/codex", "sha256": "0" * 64}
        for channel, stderr in (
            ("latest", ""),
            ("latest", process_module._CODEX_VERSION_STDERR["stable"][1]),
            ("stable", process_module._CODEX_VERSION_STDERR["latest"][0]),
        ):
            with self.subTest(channel=channel, stderr=stderr):
                output = self.root / f"preflight-{uuid.uuid4().hex}"
                version = MagicMock(
                    returncode=0, stdout="codex-cli 0.156.1\n", stderr=stderr
                )
                with (
                    patch.object(
                        TrustedSubprocessWithList, "run", return_value=version
                    ),
                    patch(
                        "testinfra.tfa.process._codex_entrypoint_identity",
                        return_value={"resolved_path": executable["path"]},
                    ),
                    patch(
                        "testinfra.tfa.process._root_owned_regular_record",
                        return_value=executable,
                    ),
                    patch(
                        "testinfra.tfa.process._codex_release_identity",
                        return_value=(channel, {"sha256": "1" * 64}),
                    ),
                    self.assertRaisesRegex(ProcessError, "invalid result"),
                ):
                    process_module._codex_environment_preflight(self.root, output, {})
                identity = json.loads((output / "identity.json").read_text())
                self.assertEqual(identity["state"], "version-failed")
                self.assertEqual(identity["release_channel"], channel)

    def test_payload_binding_requires_eligible_hash_bound_identity(self) -> None:
        binding = process_module._codex_payload_binding
        self.assertEqual(binding(None, ("--version",)), "unbound")
        eligible = {"blocker": None, "payload_sha256": "a" * 64, "state": "eligible"}
        self.assertEqual(binding(eligible, ("exec",)), "a" * 64)
        with self.assertRaisesRegex(ProcessError, "lacks a preflight identity"):
            binding(None, ("exec", "prompt"))
        for identity in (
            {**eligible, "state": "blocked"},
            {**eligible, "blocker": "blocked"},
            {**eligible, "payload_sha256": None},
            {**eligible, "payload_sha256": "A" * 64},
            {**eligible, "payload_sha256": "unbound"},
        ):
            with (
                self.subTest(identity=identity),
                self.assertRaisesRegex(ProcessError, "eligible hash-bound payload"),
            ):
                binding(identity, ("exec",))

    def test_spawn_refuses_unbound_codex_identity_before_popen(self) -> None:
        runtime = MagicMock(wrapper=Path("/wrapper"), supervisor=Path("/supervisor"))
        blocked = {
            "blocker": process_module._CODEX_SOCKET_ISOLATION_BLOCKER,
            "payload_sha256": None,
            "state": "blocked",
        }
        for identity, environment, message in (
            (blocked, {}, "eligible hash-bound payload"),
            (None, {}, "lacks a preflight identity"),
            (
                {**blocked, "blocker": None, "state": "eligible"},
                {"TFA_PAYLOAD_SHA256": "a" * 64},
                "already populated",
            ),
        ):
            with (
                self.subTest(message=message),
                patch.object(process_module, "_pinned_runtime", return_value=runtime),
                patch.object(TrustedSubprocessWithList, "Popen") as popen,
                patch.object(process_module, "_open_verified_codex_launcher") as opened,
                self.assertRaisesRegex(ProcessError, message),
            ):
                process_module._spawn_contained(
                    "/usr/local/bin/codex",
                    ("exec", "prompt"),
                    self.root,
                    MagicMock(),
                    MagicMock(),
                    environment,
                    MagicMock(),
                    self.root,
                    identity,
                )
            popen.assert_not_called()
            opened.assert_not_called()
            runtime.close.assert_called()
            runtime.close.reset_mock()

    def test_payload_sentinel_digest_requires_one_exact_digest_line(self) -> None:
        sentinel = self.root / "sentinel"
        digest = "b" * 64
        sentinel.write_text(f"1 /cgroup\npayload-sha256 {digest}\n")
        self.assertEqual(process_module._payload_sentinel_digest(sentinel), digest)
        for content, message in (
            ("1 /cgroup\n", "lacks a payload digest"),
            (f"1 /cgroup\npayload-sha256 {digest}\nextra\n", "lacks a payload digest"),
            (f"1 /cgroup\npayload-sha512 {digest}\n", "lacks a payload digest"),
            (f"1 /cgroup\npayload-sha256 {digest.upper()}\n", "malformed"),
            ("1 /cgroup\npayload-sha256 unbound\n", "malformed"),
        ):
            with self.subTest(content=content):
                sentinel.write_text(content)
                with self.assertRaisesRegex(ProcessError, message):
                    process_module._payload_sentinel_digest(sentinel)
        with self.assertRaisesRegex(ProcessError, "unreadable"):
            process_module._payload_sentinel_digest(self.root / "missing")

    def test_wrapper_binds_payload_descriptor_to_digest(self) -> None:
        wrapper = _load_payload_wrapper()
        content = b"payload\n" * 300000
        payload = self._trusted_file("payload/codex", content)
        digest = hashlib.sha256(content).hexdigest()
        for expected in (digest, "unbound"):
            with self.subTest(expected=expected):
                descriptor, value = wrapper._bind_payload(str(payload), expected)
                try:
                    self.assertEqual(value, digest)
                    self.assertTrue(os.get_inheritable(descriptor))
                    self.assertEqual(
                        os.fstat(descriptor).st_ino, payload.stat().st_ino
                    )
                finally:
                    os.close(descriptor)
        before = _open_descriptor_count()
        link = payload.with_name("link")
        link.symlink_to(payload)
        writable = self._trusted_file("payload/writable", content, mode=0o775)
        for path, expected, message in (
            (payload, "0" * 64, "differs from its reviewed digest"),
            (payload, "A" * 64, "malformed"),
            (payload, "", "malformed"),
            (link, digest, "cannot be bound"),
            (writable, digest, "differs from its reviewed digest"),
            (payload.with_name("missing"), digest, "cannot be bound"),
        ):
            with (
                self.subTest(path=path.name, expected=expected),
                self.assertRaisesRegex(SystemExit, message),
            ):
                wrapper._bind_payload(str(path), expected)
        self.assertEqual(_open_descriptor_count(), before)

    def test_wrapper_sentinel_round_trips_payload_digest(self) -> None:
        wrapper = _load_payload_wrapper()
        digest = "c" * 64
        sentinel = self.root / "bound.sentinel"
        wrapper._write_sentinel(sentinel, self.root / "cgroup", digest)
        self.assertEqual(process_module._payload_sentinel_digest(sentinel), digest)
        unbound = self.root / "plain.sentinel"
        wrapper._write_sentinel(unbound, self.root / "cgroup", None)
        self.assertEqual(len(unbound.read_text().splitlines()), 1)
        with self.assertRaisesRegex(ProcessError, "lacks a payload digest"):
            process_module._payload_sentinel_digest(unbound)

    def _run_with_fake_clock(
        self,
        timeout_seconds: float,
        startup_timeout_seconds: float | None,
        readiness: Callable[[_FakeClock], dict[str, object] | None] | None,
    ) -> tuple[dict[str, object], list[object], list[float], ProcessError | None]:
        clock = _FakeClock(1000.0)
        waits: list[object] = []
        readiness_deadlines: list[float] = []
        process = MagicMock(pid=os.getpid())
        process.poll.return_value = None

        def wait(timeout: float) -> int:
            waits.append(timeout)
            if len(waits) == 1 and timeout != 5:
                raise subprocess.TimeoutExpired("payload", timeout)
            return -9

        process.wait.side_effect = wait
        artifact_dir = self.root / f"run-{uuid.uuid4().hex}"

        def spawn(*_args: object) -> tuple[object, ...]:
            # Launch-time identity checks and wrapper hashing take time; none of
            # it may be charged before the documented deadline origin.
            clock.now += 100
            return (
                process,
                artifact_dir / "missing.sentinel",
                artifact_dir / "missing.status",
                CONTAINMENT,
                MagicMock(),
            )

        def wait_for_readiness(
            _process: object, _path: Path, deadline: float
        ) -> dict[str, object] | None:
            readiness_deadlines.append(deadline)
            return None if readiness is None else readiness(clock)

        cgroup = MagicMock(path=self.root / "cgroup")
        cgroup.terminate_and_remove.return_value = ()
        options: dict[str, object] = {}
        if startup_timeout_seconds is not None:
            options = {
                "readiness_path": self.root / f"ready-{uuid.uuid4().hex}",
                "startup_timeout_seconds": startup_timeout_seconds,
            }
        error: ProcessError | None = None
        with (
            patch.object(process_module, "time", clock),
            patch.object(process_module, "_ProcessCgroup", return_value=cgroup),
            patch.object(process_module, "_spawn_contained", side_effect=spawn),
            patch.object(
                process_module,
                "_wait_for_readiness",
                side_effect=wait_for_readiness,
            ),
        ):
            try:
                run_process(
                    "/usr/local/bin/fbpython",
                    ("-c", "pass"),
                    self.root,
                    artifact_dir,
                    timeout_seconds,
                    **options,
                )
            except ProcessError as raised:
                error = raised
        metadata = json.loads((artifact_dir / "metadata.json").read_text())
        return metadata, waits, readiness_deadlines, error

    def test_execution_deadline_starts_when_launcher_is_spawned(self) -> None:
        metadata, waits, deadlines, error = self._run_with_fake_clock(7, None, None)
        self.assertIsNone(error)
        self.assertEqual(waits, [7, 5])
        self.assertEqual(deadlines, [])
        self.assertTrue(metadata["timed_out"])
        self.assertEqual(metadata["deadline_origin"], "payload-launcher-spawned")
        self.assertIsNone(metadata["startup_timeout_seconds"])
        self.assertEqual(metadata["timeout_seconds"], 7)

    def test_readiness_is_charged_against_one_execution_deadline(self) -> None:
        def ready_after_two_seconds(clock: _FakeClock) -> dict[str, object]:
            clock.now += 2
            return {"kind": "file"}

        metadata, waits, deadlines, error = self._run_with_fake_clock(
            7, 3, ready_after_two_seconds
        )
        self.assertIsNone(error)
        self.assertEqual(deadlines, [1103])
        self.assertEqual(waits, [5, 5])
        self.assertEqual(metadata["startup_timeout_seconds"], 3)
        self.assertEqual(metadata["deadline_origin"], "payload-launcher-spawned")

    def test_startup_deadline_shorter_than_execution_is_a_readiness_error(
        self,
    ) -> None:
        metadata, waits, deadlines, error = self._run_with_fake_clock(7, 3, None)
        self.assertIsNotNone(error)
        self.assertRegex(str(error), "readiness timed out")
        self.assertEqual(deadlines, [1103])
        self.assertEqual(waits, [5])
        self.assertEqual(metadata["state"], "running")

    def test_execution_deadline_wins_when_startup_is_not_shorter(self) -> None:
        for startup in (7, 9):
            with self.subTest(startup=startup):
                metadata, waits, deadlines, error = self._run_with_fake_clock(
                    7, startup, None
                )
                self.assertIsNone(error)
                self.assertEqual(deadlines, [1107])
                self.assertEqual(waits, [5])
                self.assertTrue(metadata["timed_out"])
                self.assertEqual(metadata["startup_timeout_seconds"], startup)

    def _run_bound_codex_version(self, binding: str, recorded: str) -> Path:
        artifact_dir = self.root / f"codex-{uuid.uuid4().hex}"
        process = MagicMock(pid=os.getpid())
        process.poll.return_value = 0
        process.wait.return_value = 0

        def spawn(*_args: object) -> tuple[object, ...]:
            sentinel = artifact_dir / "payload.sentinel"
            status = artifact_dir / "payload.status"
            sentinel.write_text(f"1 /cgroup\npayload-sha256 {recorded}\n")
            status.write_text("0 0 123\n")
            os.chmod(sentinel, 0o600)
            os.chmod(status, 0o600)
            return (process, sentinel, status, CONTAINMENT, MagicMock())

        cgroup = MagicMock(path=self.root / "cgroup")
        cgroup.terminate_and_remove.return_value = ()
        with (
            patch.object(process_module, "_ProcessCgroup", return_value=cgroup),
            patch.object(process_module, "_spawn_contained", side_effect=spawn),
            patch.object(
                process_module, "_codex_payload_binding", return_value=binding
            ),
        ):
            run_process(
                "/usr/local/bin/codex", ("--version",), self.root, artifact_dir, 10
            )
        return artifact_dir

    def test_run_process_enforces_wrapper_reported_payload_digest(self) -> None:
        artifact_dir = self._run_bound_codex_version("unbound", "d" * 64)
        metadata = json.loads((artifact_dir / "metadata.json").read_text())
        self.assertEqual(metadata["payload_binding"], "unbound")
        self.assertEqual(metadata["payload_sha256"], "d" * 64)
        artifact_dir = self._run_bound_codex_version("d" * 64, "d" * 64)
        metadata = json.loads((artifact_dir / "metadata.json").read_text())
        self.assertEqual(metadata["payload_binding"], "d" * 64)
        with self.assertRaisesRegex(ProcessError, "differs from its reviewed digest"):
            self._run_bound_codex_version("e" * 64, "d" * 64)


