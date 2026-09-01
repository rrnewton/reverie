#!/usr/bin/env python3
"""Loopback and filesystem-confinement tests for the compatibility site server."""

from __future__ import annotations

import gzip
import hashlib
import os
import select
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[2]
SERVER = ROOT / "ci-hub/compatibility-website/serve.rs"
FAVICON = ROOT / "ci-hub/compatibility-website/assets/favicon.svg"
SERVER_COMMAND = ["rust-script", "--force", str(SERVER)]
START_SECONDS = 20
REQUEST_SECONDS = 3

SECURITY_HEADERS = {
    "cache-control": "no-store",
    "connection": "close",
    "cross-origin-opener-policy": "same-origin",
    "cross-origin-resource-policy": "same-origin",
    "referrer-policy": "no-referrer",
    "x-content-type-options": "nosniff",
    "x-frame-options": "DENY",
}


@dataclass(frozen=True)
class Response:
    status: int
    reason: str
    headers: dict[str, str]
    body: bytes


class RunningServer:
    def __init__(
        self,
        root: Path,
        *,
        port: int = 0,
        command: list[str] | None = None,
    ) -> None:
        self.root = root
        self.process = subprocess.Popen(
            [
                *(SERVER_COMMAND if command is None else command),
                "--root",
                str(root),
                "--port",
                str(port),
            ],
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
        try:
            assert self.process.stdout is not None
            ready, _, _ = select.select([self.process.stdout], [], [], START_SECONDS)
            if not ready:
                raise AssertionError("server did not print its ready URL")
            line = self.process.stdout.readline()
            if not line:
                stderr = self.process.stderr.read() if self.process.stderr else ""
                raise AssertionError(f"server exited before readiness: {stderr}")
            self.ready_line = line
            parsed = urlsplit(line.rstrip("\n"))
            if (
                parsed.scheme != "http"
                or parsed.hostname != "127.0.0.1"
                or parsed.path != "/"
                or parsed.query
                or parsed.fragment
                or parsed.port is None
            ):
                raise AssertionError(f"invalid ready URL: {line!r}")
            self.port = parsed.port
        except BaseException:
            self.ensure_stopped()
            raise

    def stop(
        self, stop_signal: signal.Signals = signal.SIGTERM
    ) -> tuple[int, str, str]:
        if self.process.poll() is None:
            os.killpg(self.process.pid, stop_signal)
        try:
            stdout, stderr = self.process.communicate(timeout=REQUEST_SECONDS)
        except subprocess.TimeoutExpired:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.communicate(timeout=REQUEST_SECONDS)
            raise AssertionError("server did not stop within its lifecycle bound")
        return self.process.returncode, stdout, stderr

    def ensure_stopped(self) -> None:
        if self.process.poll() is None:
            self.stop()
        else:
            self.process.communicate(timeout=REQUEST_SECONDS)

    def request(self, raw: bytes, *, shutdown_write: bool = True) -> Response:
        with socket.create_connection(
            ("127.0.0.1", self.port), timeout=REQUEST_SECONDS
        ) as connection:
            connection.settimeout(REQUEST_SECONDS)
            connection.sendall(raw)
            if shutdown_write:
                connection.shutdown(socket.SHUT_WR)
            response = bytearray()
            while True:
                chunk = connection.recv(16 * 1024)
                if not chunk:
                    break
                response.extend(chunk)
        return parse_response(bytes(response))


def parse_response(raw: bytes) -> Response:
    head, separator, body = raw.partition(b"\r\n\r\n")
    if not separator:
        raise AssertionError(f"response has no header terminator: {raw!r}")
    lines = head.decode("ascii").split("\r\n")
    version, code, reason = lines[0].split(" ", 2)
    if version != "HTTP/1.1":
        raise AssertionError(f"unexpected response version {version!r}")
    headers: dict[str, str] = {}
    for line in lines[1:]:
        name, value = line.split(":", 1)
        lowered = name.lower()
        if lowered in headers:
            raise AssertionError(f"duplicate response header {lowered!r}")
        headers[lowered] = value.lstrip(" ")
    return Response(int(code), reason, headers, body)


def request_bytes(
    port: int,
    target: str = "/",
    *,
    method: str = "GET",
    version: str = "HTTP/1.1",
    headers: tuple[tuple[str, str], ...] = (),
    host: bool = True,
    suffix: bytes = b"",
) -> bytes:
    fields = [("Host", f"127.0.0.1:{port}")] if host else []
    fields.extend(headers)
    encoded_headers = "".join(f"{name}: {value}\r\n" for name, value in fields)
    return (
        f"{method} {target} {version}\r\n{encoded_headers}\r\n".encode("ascii") + suffix
    )


def assert_common_headers(test: unittest.TestCase, response: Response) -> None:
    for name, value in SECURITY_HEADERS.items():
        test.assertEqual(response.headers.get(name), value, name)
    test.assertIn("content-security-policy", response.headers)
    policy = response.headers["content-security-policy"]
    test.assertIn("default-src 'none'", policy)
    test.assertIn("script-src 'self'", policy)
    test.assertIn("style-src 'self'", policy)
    test.assertIn("img-src 'self'", policy)
    test.assertNotIn("data:", policy)
    test.assertIn("connect-src 'self'", policy)
    test.assertIn("frame-ancestors 'none'", policy)
    test.assertNotIn("strict-transport-security", response.headers)
    test.assertEqual(int(response.headers["content-length"]), len(response.body))


def tree_snapshot(root: Path) -> tuple[tuple[str, str, int, str], ...]:
    rows = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        metadata = path.lstat()
        if path.is_symlink():
            kind = "symlink"
            digest = os.readlink(path)
        elif path.is_dir():
            kind = "directory"
            digest = ""
        elif path.is_file():
            kind = "file"
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
        else:
            kind = "special"
            digest = ""
        rows.append((relative, kind, metadata.st_mode, digest))
    return tuple(rows)


class CompatibilityWebsiteServeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        result = subprocess.run(
            [*SERVER_COMMAND, "--help"],
            cwd=ROOT,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=120,
        )
        if result.returncode != 0:
            raise AssertionError(result.stderr)

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.temp = Path(self.temporary.name)
        self.site = self.temp / "site"
        (self.site / "assets").mkdir(parents=True)
        (self.site / "data").mkdir()
        (self.site / "logs").mkdir()
        (self.site / "cells").mkdir()
        self.files = {
            "index.html": b"<!doctype html><title>Compatibility</title>",
            "build.json": b'{"schema":"compatibility-site-build/v1"}\n',
            "assets/favicon.svg": FAVICON.read_bytes(),
            "assets/site.css": b"body { color: black; }\n",
            "assets/site.js": b"document.body.dataset.ready = 'yes';\n",
            "cells/example.html": b"<!doctype html><title>Cell</title>",
            "data/cells.json.gz": gzip.compress(b'{"cells":[]}\n', mtime=0),
            f"logs/{'a' * 64}.log.gz": gzip.compress(b"one log line\n", mtime=0),
        }
        for relative, content in self.files.items():
            path = self.site / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def start(self, *, root: Path | None = None, port: int = 0) -> RunningServer:
        server = RunningServer(self.site if root is None else root, port=port)
        self.addCleanup(server.ensure_stopped)
        return server

    def assert_response(
        self,
        response: Response,
        status: int,
        *,
        body: bytes | None = None,
    ) -> None:
        self.assertEqual(response.status, status, response)
        assert_common_headers(self, response)
        if body is not None:
            self.assertEqual(response.body, body)

    def test_help_argument_errors_and_startup_errors_are_distinct(self) -> None:
        help_result = subprocess.run(
            [*SERVER_COMMAND, "--help"],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(help_result.returncode, 0)
        self.assertTrue(help_result.stdout.startswith("Usage:"))
        self.assertIn("default port is 0", help_result.stdout)
        self.assertEqual(help_result.stderr, "")
        self.assertNotIn("TODO", help_result.stdout)

        for arguments in (
            (),
            ("--root", str(self.site), "--root", str(self.site)),
            ("--root", str(self.site), "--port", "65536"),
            ("--root", str(self.site), "--bind", "::1"),
        ):
            with self.subTest(arguments=arguments):
                result = subprocess.run(
                    [*SERVER_COMMAND, *arguments],
                    cwd=ROOT,
                    check=False,
                    capture_output=True,
                    text=True,
                    timeout=120,
                )
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")
                self.assertIn("Usage:", result.stderr)

        missing = subprocess.run(
            [*SERVER_COMMAND, "--root", str(self.temp / "missing")],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(missing.returncode, 1)
        self.assertEqual(missing.stdout, "")
        self.assertIn("cannot open website root", missing.stderr)

    def test_failed_ready_line_validation_reaps_its_process_group(self) -> None:
        pid_file = self.temp / "bad-ready.pid"
        fake = self.temp / "bad-ready.py"
        fake.write_text(
            "import os, pathlib, time\n"
            f"pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid()))\n"
            "print('not-a-loopback-url', flush=True)\n"
            "time.sleep(30)\n"
        )
        with self.assertRaisesRegex(AssertionError, "invalid ready URL"):
            RunningServer(self.site, command=[sys.executable, str(fake)])
        pid = int(pid_file.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_root_get_mime_gzip_query_and_head(self) -> None:
        server = self.start()
        try:
            cases = (
                ("/", "text/html; charset=utf-8", None, self.files["index.html"]),
                (
                    "/build.json",
                    "application/json",
                    None,
                    self.files["build.json"],
                ),
                (
                    "/assets/favicon.svg",
                    "image/svg+xml",
                    None,
                    self.files["assets/favicon.svg"],
                ),
                (
                    "/assets/site.css",
                    "text/css; charset=utf-8",
                    None,
                    self.files["assets/site.css"],
                ),
                (
                    "/assets/site.js",
                    "text/javascript; charset=utf-8",
                    None,
                    self.files["assets/site.js"],
                ),
                (
                    "/data/cells.json.gz",
                    "application/json",
                    "gzip",
                    self.files["data/cells.json.gz"],
                ),
                (
                    f"/logs/{'a' * 64}.log.gz",
                    "text/plain; charset=utf-8",
                    "gzip",
                    self.files[f"logs/{'a' * 64}.log.gz"],
                ),
            )
            for target, content_type, encoding, body in cases:
                with self.subTest(target=target):
                    response = server.request(request_bytes(server.port, target))
                    self.assert_response(response, 200, body=body)
                    self.assertEqual(response.headers["content-type"], content_type)
                    self.assertEqual(response.headers.get("content-encoding"), encoding)

            queried = server.request(request_bytes(server.port, "/?q=%zz"))
            self.assert_response(queried, 200, body=self.files["index.html"])

            ranged = server.request(
                request_bytes(server.port, headers=(("Range", "bytes=0-3"),))
            )
            self.assert_response(ranged, 200, body=self.files["index.html"])
            self.assertNotIn("content-range", ranged.headers)

            get_response = server.request(
                request_bytes(server.port, "/cells/example.html")
            )
            head_response = server.request(
                request_bytes(server.port, "/cells/example.html", method="HEAD")
            )
            self.assert_response(
                get_response, 200, body=self.files["cells/example.html"]
            )
            self.assertEqual(head_response.status, get_response.status)
            self.assertEqual(head_response.headers, get_response.headers)
            self.assertEqual(head_response.body, b"")
            self.assertEqual(
                int(head_response.headers["content-length"]),
                len(self.files["cells/example.html"]),
            )

            icon_get = server.request(request_bytes(server.port, "/assets/favicon.svg"))
            icon_head = server.request(
                request_bytes(server.port, "/assets/favicon.svg", method="HEAD")
            )
            self.assert_response(icon_get, 200, body=self.files["assets/favicon.svg"])
            self.assertEqual(icon_get.headers["content-type"], "image/svg+xml")
            self.assertEqual(icon_head.status, 200)
            self.assertEqual(icon_head.headers, icon_get.headers)
            self.assertEqual(icon_head.body, b"")
        finally:
            returncode, stdout, stderr = server.stop()
            self.assertEqual((returncode, stdout, stderr), (0, "", ""))

    def test_http_10_host_rules_and_http_11_exact_host(self) -> None:
        server = self.start()
        try:
            http_10 = server.request(
                request_bytes(server.port, version="HTTP/1.0", host=False)
            )
            self.assert_response(http_10, 200, body=self.files["index.html"])

            cases = (
                request_bytes(server.port, host=False),
                request_bytes(
                    server.port,
                    headers=(("Host", f"127.0.0.1:{server.port}"),),
                ),
                request_bytes(
                    server.port,
                    headers=(("Host", "localhost"),),
                    host=False,
                ),
                request_bytes(
                    server.port,
                    version="HTTP/1.0",
                    headers=(("Host", "localhost"),),
                    host=False,
                ),
                request_bytes(
                    server.port,
                    headers=(("Host", "127.0.0.1"),),
                    host=False,
                ),
                request_bytes(
                    server.port,
                    headers=(("Host", f"127.0.0.1:{server.port + 1}"),),
                    host=False,
                ),
                request_bytes(
                    server.port,
                    headers=(("Host", f"127.0.0.2:{server.port}"),),
                    host=False,
                ),
                request_bytes(
                    server.port,
                    headers=(("Host", f"user@127.0.0.1:{server.port}"),),
                    host=False,
                ),
                request_bytes(
                    server.port,
                    headers=(("Host", f" 127.0.0.1:{server.port}"),),
                    host=False,
                ),
                request_bytes(
                    server.port,
                    headers=(("Host", f"127.0.0.1:{server.port} "),),
                    host=False,
                ),
            )
            for raw in cases:
                response = server.request(raw)
                self.assert_response(response, 400, body=b"Bad Request\n")
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_port_80_accepts_the_standard_bare_ipv4_host(self) -> None:
        reservation = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            reservation.bind(("127.0.0.1", 80))
        except OSError as error:
            reservation.close()
            self.skipTest(f"loopback port 80 unavailable: {error}")
        reservation.close()
        server = self.start(port=80)
        try:
            response = server.request(
                request_bytes(
                    server.port,
                    headers=(("Host", "127.0.0.1"),),
                    host=False,
                )
            )
            self.assert_response(response, 200, body=self.files["index.html"])
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_unsafe_paths_are_indistinguishable_404s(self) -> None:
        outside = self.temp / "outside.html"
        outside.write_bytes(b"secret")
        server = self.start()
        try:
            targets = (
                "/../outside.html",
                "/%2e%2e/outside.html",
                "/%2E./outside.html",
                "/cells%2fexample.html",
                "/cells%5cexample.html",
                "/cells\\example.html",
                "/%252e%252e/outside.html",
                "/cells//example.html",
                "/./index.html",
                "/%00.html",
                "/%1f.html",
                "/%",
                "/%zz",
                "http://127.0.0.1/index.html",
                "/index.html#fragment",
            )
            bodies = set()
            for target in targets:
                with self.subTest(target=target):
                    response = server.request(request_bytes(server.port, target))
                    self.assert_response(response, 404, body=b"Not Found\n")
                    bodies.add(response.body)
            self.assertEqual(bodies, {b"Not Found\n"})
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_symlink_components_and_root_replacement_cannot_escape(self) -> None:
        outside = self.temp / "outside"
        outside.mkdir()
        (outside / "secret.html").write_bytes(b"outside")
        (self.site / "linked").symlink_to(outside, target_is_directory=True)
        (self.site / "final.html").symlink_to(outside / "secret.html")
        before = tree_snapshot(self.site)
        server = self.start()
        try:
            intermediate = server.request(
                request_bytes(server.port, "/linked/secret.html")
            )
            final = server.request(request_bytes(server.port, "/final.html"))
            self.assert_response(intermediate, 404, body=b"Not Found\n")
            self.assert_response(final, 404, body=b"Not Found\n")

            original = self.temp / "original-site"
            self.site.rename(original)
            self.site.symlink_to(outside, target_is_directory=True)
            retained = server.request(request_bytes(server.port))
            self.assert_response(retained, 200, body=self.files["index.html"])
            self.assertEqual(tree_snapshot(original), before)
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

        root_link = self.temp / "root-link"
        root_link.symlink_to(outside, target_is_directory=True)
        result = subprocess.run(
            [*SERVER_COMMAND, "--root", str(root_link)],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("cannot open website root", result.stderr)

        real_parent = self.temp / "real-parent"
        real_root = real_parent / "nested-site"
        real_root.mkdir(parents=True)
        (real_root / "index.html").write_bytes(b"not reachable through a root symlink")
        linked_parent = self.temp / "linked-parent"
        linked_parent.symlink_to(real_parent, target_is_directory=True)
        result = subprocess.run(
            [*SERVER_COMMAND, "--root", str(linked_parent / "nested-site")],
            cwd=ROOT,
            check=False,
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("cannot open website root", result.stderr)

    def test_concurrent_intermediate_component_swap_never_serves_outside(self) -> None:
        inside = self.site / "race"
        held = self.site / "race-held"
        outside = self.temp / "race-outside"
        inside.mkdir()
        outside.mkdir()
        (outside / "secret.html").write_bytes(b"outside-secret")
        stop = threading.Event()
        errors: list[BaseException] = []

        def swap_component() -> None:
            try:
                while not stop.is_set():
                    inside.rename(held)
                    inside.symlink_to(outside, target_is_directory=True)
                    time.sleep(0.0005)
                    inside.unlink()
                    held.rename(inside)
            except BaseException as error:  # surfaced in the owning test thread
                errors.append(error)

        server = self.start()
        swapper = threading.Thread(target=swap_component)
        swapper.start()
        try:
            for _index in range(80):
                response = server.request(
                    request_bytes(server.port, "/race/secret.html")
                )
                self.assert_response(response, 404, body=b"Not Found\n")
                self.assertNotIn(b"outside-secret", response.body)
        finally:
            stop.set()
            swapper.join(timeout=REQUEST_SECONDS * 2)
            stopped = not swapper.is_alive()
            if inside.is_symlink():
                inside.unlink()
            if held.exists():
                held.rename(inside)
            server_result = server.stop()
        self.assertTrue(stopped)
        self.assertEqual(server_result, (0, "", ""))
        self.assertEqual(errors, [])

    def test_nonregular_and_unapproved_file_types_are_404(self) -> None:
        (self.site / "directory.html").mkdir()
        os.mkfifo(self.site / "fifo.html")
        socket_path = self.site / "socket.html"
        unix_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        unix_listener.bind(str(socket_path))
        (self.site / "data/raw.json").write_bytes(b"{}\n")
        (self.site / "logs/raw.log").write_bytes(b"text\n")
        (self.site / "logs/abcd.log.gz").write_bytes(b"short digest")
        (self.site / f"logs/{'A' * 64}.log.gz").write_bytes(b"uppercase digest")
        (self.site / "cells/wrong.json.gz").write_bytes(b"wrong directory")
        (self.site / "assets/wrong.log.gz").write_bytes(b"wrong directory")
        (self.site / "favicon.svg").write_bytes(b"wrong SVG path")
        (self.site / "assets/other.svg").write_bytes(b"wrong SVG name")
        (self.site / "cells/favicon.svg").write_bytes(b"wrong SVG directory")
        (self.site / "logs/allowed.html").write_bytes(b"html is suffix-allowed")
        (self.site / "cells/allowed.css").write_bytes(b"css is suffix-allowed")
        (self.site / "data/allowed.js").write_bytes(b"js is suffix-allowed")
        (self.site / "readme.txt").write_bytes(b"text\n")
        server = self.start()
        try:
            for target in (
                "/directory.html",
                "/fifo.html",
                "/socket.html",
                "/data/raw.json",
                "/logs/raw.log",
                "/logs/abcd.log.gz",
                f"/logs/{'A' * 64}.log.gz",
                "/cells/wrong.json.gz",
                "/assets/wrong.log.gz",
                "/favicon.svg",
                "/assets/other.svg",
                "/cells/favicon.svg",
                "/readme.txt",
                "/cells",
                "/missing.html",
            ):
                with self.subTest(target=target):
                    response = server.request(request_bytes(server.port, target))
                    self.assert_response(response, 404, body=b"Not Found\n")
        finally:
            self.assertEqual(server.stop(), (0, "", ""))
            unix_listener.close()

        server = self.start()
        try:
            for target, content_type in (
                ("/logs/allowed.html", "text/html; charset=utf-8"),
                ("/cells/allowed.css", "text/css; charset=utf-8"),
                ("/data/allowed.js", "text/javascript; charset=utf-8"),
            ):
                with self.subTest(target=target):
                    response = server.request(request_bytes(server.port, target))
                    self.assert_response(response, 200)
                    self.assertEqual(response.headers["content-type"], content_type)
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_malformed_method_version_and_framing_requests_are_bounded(self) -> None:
        server = self.start()
        try:
            cases = (
                (b"GET / HTTP/1.1\nHost: x\n\n", 400),
                (b"GET\t/\tHTTP/1.1\r\n\r\n", 400),
                (b"GET / HTTP/1.1 extra\r\n\r\n", 400),
                (request_bytes(server.port, method="POST"), 405),
                (request_bytes(server.port, version="HTTP/2.0"), 505),
                (request_bytes(server.port, version="NOTHTTP"), 400),
                (
                    request_bytes(server.port, headers=(("Content-Length", "0"),)),
                    400,
                ),
                (
                    request_bytes(
                        server.port, headers=(("Transfer-Encoding", "chunked"),)
                    ),
                    400,
                ),
                (
                    request_bytes(server.port, headers=(("Expect", "100-continue"),)),
                    400,
                ),
                (request_bytes(server.port, suffix=b"body"), 400),
                (
                    request_bytes(
                        server.port,
                        suffix=request_bytes(server.port),
                    ),
                    400,
                ),
                (
                    request_bytes(
                        server.port,
                        headers=(("X-Test", "one"), ("X-Test", "two")),
                    ),
                    400,
                ),
            )
            for raw, expected in cases:
                with self.subTest(expected=expected, raw=raw[:60]):
                    response = server.request(raw)
                    self.assert_response(response, expected)
                    if expected == 405:
                        self.assertEqual(response.headers.get("allow"), "GET, HEAD")

            oversized = (
                f"GET / HTTP/1.1\r\nHost: 127.0.0.1:{server.port}\r\nX: ".encode()
                + b"a" * (16 * 1024)
                + b"\r\n\r\n"
            )
            response = server.request(oversized)
            self.assert_response(response, 400, body=b"Bad Request\n")
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_large_gzip_log_streams_fully_and_ignores_range(self) -> None:
        name = f"{'b' * 64}.log.gz"
        body = gzip.compress(os.urandom(3 * 1024 * 1024), mtime=0)
        (self.site / "logs" / name).write_bytes(body)
        server = self.start()
        try:
            response = server.request(
                request_bytes(
                    server.port,
                    f"/logs/{name}",
                    headers=(("Range", "bytes=123-456"),),
                )
            )
            self.assert_response(response, 200, body=body)
            self.assertEqual(response.headers["content-encoding"], "gzip")
            self.assertNotIn("content-range", response.headers)
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_delayed_second_request_gets_no_second_response(self) -> None:
        server = self.start()
        try:
            with socket.create_connection(
                ("127.0.0.1", server.port), timeout=REQUEST_SECONDS
            ) as connection:
                connection.settimeout(REQUEST_SECONDS)
                connection.sendall(request_bytes(server.port))
                time.sleep(0.1)
                try:
                    connection.sendall(
                        request_bytes(server.port, "/cells/example.html")
                    )
                except OSError:
                    pass
                raw = bytearray()
                while True:
                    chunk = connection.recv(16 * 1024)
                    if not chunk:
                        break
                    raw.extend(chunk)
            self.assertEqual(raw.count(b"HTTP/1.1 "), 1)
            self.assert_response(
                parse_response(bytes(raw)), 200, body=self.files["index.html"]
            )
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_head_errors_have_get_headers_and_no_body(self) -> None:
        server = self.start()
        try:
            get_response = server.request(
                request_bytes(server.port, "/missing.html", method="GET")
            )
            head_response = server.request(
                request_bytes(server.port, "/missing.html", method="HEAD")
            )
            self.assertEqual(get_response.status, 404)
            self.assertEqual(head_response.status, 404)
            self.assertEqual(get_response.headers, head_response.headers)
            self.assertEqual(get_response.body, b"Not Found\n")
            self.assertEqual(head_response.body, b"")
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_timeout_and_partial_disconnect_do_not_stop_server(self) -> None:
        server = self.start()
        try:
            with socket.create_connection(
                ("127.0.0.1", server.port), timeout=REQUEST_SECONDS
            ) as connection:
                connection.settimeout(REQUEST_SECONDS)
                connection.sendall(b"GET / HTTP/1.1\r\n")
                raw = bytearray()
                while True:
                    chunk = connection.recv(4096)
                    if not chunk:
                        break
                    raw.extend(chunk)
            timeout_response = parse_response(bytes(raw))
            self.assert_response(timeout_response, 400, body=b"Bad Request\n")

            connection = socket.create_connection(
                ("127.0.0.1", server.port), timeout=REQUEST_SECONDS
            )
            connection.sendall(b"GET / HTTP/1.1\r\n")
            connection.close()
            time.sleep(0.05)

            healthy = server.request(request_bytes(server.port))
            self.assert_response(healthy, 200, body=self.files["index.html"])
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_slow_header_trickle_hits_one_total_deadline(self) -> None:
        server = self.start()
        started = time.monotonic()
        try:
            with socket.create_connection(
                ("127.0.0.1", server.port), timeout=REQUEST_SECONDS
            ) as connection:
                connection.settimeout(REQUEST_SECONDS)
                connection.sendall(b"G")
                time.sleep(0.75)
                connection.sendall(b"E")
                raw = bytearray()
                while True:
                    chunk = connection.recv(4096)
                    if not chunk:
                        break
                    raw.extend(chunk)
            self.assertLess(time.monotonic() - started, 1.5)
            self.assert_response(parse_response(bytes(raw)), 400, body=b"Bad Request\n")
            healthy = server.request(request_bytes(server.port))
            self.assert_response(healthy, 200, body=self.files["index.html"])
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_ready_line_ipv4_explicit_port_and_no_extra_stdout(self) -> None:
        reservation = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
        reservation.close()
        server = self.start(port=port)
        try:
            self.assertEqual(server.ready_line, f"http://127.0.0.1:{port}/\n")
            assert server.process.stdout is not None
            extra, _, _ = select.select([server.process.stdout], [], [], 0.1)
            self.assertEqual(extra, [])
            with self.assertRaises(OSError):
                socket.create_connection(("::1", port), timeout=0.1)
        finally:
            self.assertEqual(server.stop(), (0, "", ""))

    def test_port_conflict_refuses_without_ready_output(self) -> None:
        reservation = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        reservation.bind(("127.0.0.1", 0))
        reservation.listen(1)
        port = reservation.getsockname()[1]
        try:
            result = subprocess.run(
                [*SERVER_COMMAND, "--root", str(self.site), "--port", str(port)],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
                timeout=120,
            )
        finally:
            reservation.close()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("cannot bind 127.0.0.1", result.stderr)

    def test_sigint_and_sigterm_exit_cleanly(self) -> None:
        for stop_signal in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(stop_signal=stop_signal):
                server = self.start()
                response = server.request(request_bytes(server.port))
                self.assert_response(response, 200, body=self.files["index.html"])
                self.assertEqual(server.stop(stop_signal), (0, "", ""))

    def test_sigterm_interrupts_a_stalled_partial_request_within_read_bound(
        self,
    ) -> None:
        server = self.start()
        connection = socket.create_connection(
            ("127.0.0.1", server.port), timeout=REQUEST_SECONDS
        )
        connection.sendall(b"GET / HTTP/1.1\r\n")
        started = time.monotonic()
        try:
            result = server.stop(signal.SIGTERM)
        finally:
            connection.close()
        self.assertLess(time.monotonic() - started, 2.0)
        self.assertEqual(result, (0, "", ""))

    def test_sigterm_interrupts_a_backpressured_large_response(self) -> None:
        large = self.site / "backpressured.html"
        with large.open("wb") as output:
            output.truncate(256 * 1024 * 1024)
        server = self.start()
        connection = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        connection.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
        connection.settimeout(REQUEST_SECONDS)
        connection.connect(("127.0.0.1", server.port))
        connection.sendall(request_bytes(server.port, "/backpressured.html"))
        prefix = connection.recv(4096)
        self.assertTrue(prefix.startswith(b"HTTP/1.1 200 OK\r\n"))
        self.assertIn(b"Content-Length: 268435456\r\n", prefix)
        started = time.monotonic()
        try:
            result = server.stop(signal.SIGTERM)
        finally:
            connection.close()
        self.assertLess(time.monotonic() - started, 3.0)
        self.assertEqual(result, (0, "", ""))
        with self.assertRaises(ProcessLookupError):
            os.kill(server.process.pid, 0)

    def test_serving_is_read_only_and_source_has_no_outbound_client(self) -> None:
        before = tree_snapshot(self.site)
        server = self.start()
        try:
            for target in ("/", "/assets/site.css", "/data/cells.json.gz"):
                self.assertEqual(
                    server.request(request_bytes(server.port, target)).status,
                    200,
                )
        finally:
            self.assertEqual(server.stop(), (0, "", ""))
        self.assertEqual(tree_snapshot(self.site), before)

        source = SERVER.read_text()
        for forbidden in (
            "TcpStream::connect",
            "UdpSocket",
            "reqwest",
            "curl",
            "Command::new",
            "fs::write",
            "File::create",
            "canonicalize",
            "read_to_end",
            "thread::spawn",
        ):
            self.assertNotIn(forbidden, source)
        self.assertEqual(source.count("libc::open("), 1)
        self.assertIn("libc::openat", source)
        self.assertIn("libc::O_NOFOLLOW", source)
        self.assertIn("libc::O_DIRECTORY", source)
        self.assertIn("libc::O_NONBLOCK", source)
        self.assertIn("TcpListener::bind((Ipv4Addr::LOCALHOST", source)
        self.assertNotIn("write_all(", source)
        self.assertIn('(port == 80 && host == "127.0.0.1")', source)
        self.assertIn('parent == Some(b"assets") && name == b"favicon.svg"', source)

        file_response = source[
            source.index("fn write_file_response(") : source.index("fn write_error(")
        ]
        write_loop = source[source.index("fn write_all_before(") :]
        self.assertEqual(file_response.count("Instant::now() + RESPONSE_DEADLINE"), 1)
        self.assertIn(
            "write_all_before(stream, &buffer[..count], deadline)", file_response
        )
        self.assertIn("STOP_REQUESTED.load(Ordering::Relaxed)", write_loop)
        self.assertIn("checked_duration_since(Instant::now())", write_loop)
        self.assertIn("remaining.min(WRITE_WAIT)", write_loop)
        self.assertIn("set_write_timeout(Some(wait))", write_loop)
        self.assertIn("io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut", write_loop)


if __name__ == "__main__":
    unittest.main()
