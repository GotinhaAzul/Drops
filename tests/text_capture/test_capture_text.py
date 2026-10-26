#!/usr/bin/python
"""Tests for scripts/capture_text.py.

Run with the system interpreter (the one with PyGObject/AT-SPI)::

    /usr/bin/python -m unittest discover -s tests/text_capture -v

The fixture tests use fake AT-SPI tree objects, so they need no desktop.  The
live tests use only disposable resources: a private tmux server with a client
attached under this test process, and a throw-away GTK 4 window whose title is
unique to the run.  They skip when that capability is unavailable.
"""

from __future__ import annotations

import glob
import json
import os
import pty
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts"
SCRIPT = SCRIPTS / "capture_text.py"
sys.path.insert(0, str(SCRIPTS))

import capture_text  # noqa: E402


try:  # System AT-SPI bindings are optional for fixture-only runs.
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi

    HAVE_ATSPI = True
    ATSPI_ERROR = ""
except Exception as exc:  # noqa: BLE001 - skip live tests honestly
    Atspi = None  # type: ignore[assignment]
    HAVE_ATSPI = False
    ATSPI_ERROR = f"{type(exc).__name__}: {exc}"

try:
    gi.require_version("Gtk", "4.0")

    HAVE_GTK4 = True
except Exception:
    HAVE_GTK4 = False


# ---------------------------------------------------------------------------
# Fake AT-SPI tree
# ---------------------------------------------------------------------------


@dataclass
class FakeNode:
    name: str = ""
    role: str = "panel"
    pid: int = 0
    text: Optional[str] = None
    children: list[FakeNode] = field(default_factory=list)
    states: set[str] = field(default_factory=set)
    attrs: dict[str, str] = field(default_factory=dict)


class FakeBackend:
    """The slice of the AT-SPI backend that capture_text uses."""

    def __init__(self, apps: list[FakeNode]) -> None:
        self.apps = apps

    def roots(self) -> list[FakeNode]:
        return self.apps

    def name(self, node: FakeNode) -> str:
        return node.name

    def role(self, node: FakeNode) -> str:
        return node.role

    def pid(self, node: FakeNode) -> int:
        return node.pid

    def child_count(self, node: FakeNode) -> int:
        return len(node.children)

    def child(self, node: FakeNode, index: int) -> Optional[FakeNode]:
        if 0 <= index < len(node.children):
            return node.children[index]
        return None

    def states(self, node: FakeNode) -> set[str]:
        return set(node.states)

    def attributes(self, node: FakeNode) -> dict[str, str]:
        return dict(node.attrs)

    def text(self, node: FakeNode, limit: int) -> Optional[tuple[str, int]]:
        if node.text is None:
            return None
        return node.text[:limit], len(node.text)


def make_frame(
    title: str,
    children: list[FakeNode],
    pid: int,
    *,
    role: str = "frame",
) -> FakeNode:
    return FakeNode(name=title, role=role, pid=pid, children=children)


def make_app(pid: int, frames: list[FakeNode], name: str = "app") -> FakeNode:
    return FakeNode(name=name, role="application", pid=pid, children=frames)


def make_window(pid: Any, title: Any, cls: str = "test") -> dict[str, Any]:
    return {
        "kind": "window",
        "window": {
            "pid": pid,
            "title": title,
            "class": cls,
            "geometry": {"x": 0, "y": 0, "width": 800, "height": 600},
        },
        "process": {"pid": pid, "cwd": "/tmp", "cmdline": ["test-app"]},
    }


def capture_fixture(
    context: dict[str, Any],
    backend: FakeBackend,
    *,
    limits: capture_text.Limits = capture_text.DEFAULT_LIMITS,
) -> dict[str, Any]:
    return capture_text.run_capture(
        context, backend=backend, limits=limits, enable_tmux=False
    )


def source_text(result: dict[str, Any]) -> str:
    return "\n".join(source.get("text", "") for source in result["sources"])


# ---------------------------------------------------------------------------
# Fixture tests
# ---------------------------------------------------------------------------


class FixtureCaptureTests(unittest.TestCase):
    def test_exact_title_selects_only_that_top_level_frame(self) -> None:
        target = make_frame(
            "Anchored Window",
            [FakeNode(name="target-label", role="label", text="inside target")],
            pid=42,
        )
        other = make_frame(
            "Other Window",
            [FakeNode(name="other-label", role="label", text="outside target")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [target, other])])

        result = capture_fixture(make_window(42, "Anchored Window"), backend)

        self.assertEqual([source["kind"] for source in result["sources"]], ["accessibility"])
        self.assertEqual(result["sources"][0]["scope"], "matched-window")
        self.assertEqual(result["sources"][0]["match"], "exact-title")
        text = source_text(result)
        self.assertIn("inside target", text)
        self.assertNotIn("outside target", text)

    def test_pid_is_a_hard_gate(self) -> None:
        other_app = make_app(
            99,
            [
                make_frame(
                    "Anchored Window",
                    [FakeNode(role="label", text="other process text")],
                    pid=99,
                )
            ],
        )
        backend = FakeBackend([other_app])

        result = capture_fixture(make_window(42, "Anchored Window"), backend)

        self.assertEqual(result["sources"], [])
        self.assertNotIn("other process text", source_text(result))
        self.assertTrue(
            any("pid" in warning and "42" in warning for warning in result["warnings"])
        )

    def test_ambiguous_exact_titles_return_nothing(self) -> None:
        first = make_frame(
            "Duplicate", [FakeNode(role="label", text="first body")], pid=42
        )
        second = make_frame(
            "Duplicate", [FakeNode(role="label", text="second body")], pid=42
        )
        backend = FakeBackend([make_app(42, [first, second])])

        result = capture_fixture(make_window(42, "Duplicate"), backend)

        self.assertEqual(result["sources"], [])
        self.assertNotIn("first body", source_text(result))
        self.assertNotIn("second body", source_text(result))
        self.assertTrue(
            any("ambiguous" in warning for warning in result["warnings"])
        )

    def test_whitespace_normalized_title_is_accepted_with_warning(self) -> None:
        target = make_frame(
            "Target window",
            [FakeNode(role="label", text="normalized body")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [target])])

        result = capture_fixture(make_window(42, "Target   window"), backend)

        self.assertIn("normalized body", source_text(result))
        self.assertEqual(result["sources"][0]["match"], "normalized-title")
        self.assertTrue(
            any("normalization" in warning for warning in result["warnings"])
        )

    def test_case_difference_is_not_a_match(self) -> None:
        target = make_frame(
            "Target Window",
            [FakeNode(role="label", text="case body")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [target])])

        result = capture_fixture(make_window(42, "target window"), backend)

        self.assertEqual(result["sources"], [])
        self.assertNotIn("case body", source_text(result))

    def test_password_role_skips_the_whole_subtree(self) -> None:
        password = FakeNode(
            name="password",
            role="password text",
            text="masked-secret",
            children=[
                FakeNode(role="label", text="nested-secret")
            ],
        )
        frame = make_frame(
            "Secret Window",
            [
                FakeNode(role="label", text="public before"),
                password,
                FakeNode(role="label", text="public after"),
            ],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(make_window(42, "Secret Window"), backend)

        text = source_text(result)
        self.assertIn("public before", text)
        self.assertIn("public after", text)
        self.assertNotIn("masked-secret", text)
        self.assertNotIn("nested-secret", text)
        self.assertTrue(
            any("password" in warning for warning in result["warnings"])
        )

    def test_protected_state_and_password_attribute_are_skipped(self) -> None:
        frame = make_frame(
            "Protected Window",
            [
                FakeNode(role="label", text="safe one"),
                FakeNode(
                    role="entry",
                    text="state secret",
                    states={"protected"},
                ),
                FakeNode(
                    role="entry",
                    text="attribute secret",
                    attrs={"text-input-type": "password"},
                ),
                FakeNode(role="label", text="safe two"),
            ],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(make_window(42, "Protected Window"), backend)

        text = source_text(result)
        self.assertIn("safe one", text)
        self.assertIn("safe two", text)
        self.assertNotIn("state secret", text)
        self.assertNotIn("attribute secret", text)

    def test_text_is_truncated_at_the_limit(self) -> None:
        frame = make_frame(
            "Long Window",
            [FakeNode(role="text", text="abcdefghijklmnopqrstuvwxyz")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(
            make_window(42, "Long Window"),
            backend,
            limits=capture_text.Limits(max_text_chars=10),
        )

        self.assertEqual(result["sources"][0]["text"], "abcdefghij")
        self.assertTrue(
            any("truncated" in warning for warning in result["warnings"])
        )

    def test_node_limit_is_a_bound_not_a_crash(self) -> None:
        frame = make_frame(
            "Node Window",
            [FakeNode(role="label", text="never visited")],
            pid=42,
        )
        frame.text = "root text"
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(
            make_window(42, "Node Window"),
            backend,
            limits=capture_text.Limits(max_nodes=1),
        )

        self.assertIn("root text", source_text(result))
        self.assertNotIn("never visited", source_text(result))
        self.assertTrue(
            any("node limit" in warning for warning in result["warnings"])
        )

    def test_depth_limit_is_a_bound_not_a_crash(self) -> None:
        deep = FakeNode(role="label", text="deep text")
        panel = FakeNode(role="panel", text="shallow text", children=[deep])
        frame = make_frame("Depth Window", [panel], pid=42)
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(
            make_window(42, "Depth Window"),
            backend,
            limits=capture_text.Limits(max_depth=1),
        )

        self.assertIn("shallow text", source_text(result))
        self.assertNotIn("deep text", source_text(result))
        self.assertTrue(
            any("depth limit" in warning for warning in result["warnings"])
        )

    def test_parent_and_child_text_is_not_duplicated(self) -> None:
        frame = make_frame(
            "Nested Window",
            [FakeNode(role="label", text="beta")],
            pid=42,
        )
        frame.text = "alpha beta"
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(make_window(42, "Nested Window"), backend)

        text = source_text(result)
        self.assertEqual(text.count("beta"), 1)
        self.assertIn("alpha beta", text)

    def test_content_name_is_used_when_text_interface_is_absent(self) -> None:
        frame = make_frame(
            "Name Window",
            [FakeNode(name="Section heading", role="heading", text=None)],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(make_window(42, "Name Window"), backend)

        self.assertIn("Section heading", source_text(result))

    def test_frame_claiming_another_pid_is_never_read(self) -> None:
        frame = make_frame(
            "Foreign Window",
            [FakeNode(role="label", text="foreign text")],
            pid=99,
        )
        backend = FakeBackend([make_app(42, [frame])])

        result = capture_fixture(make_window(42, "Foreign Window"), backend)

        self.assertEqual(result["sources"], [])
        self.assertNotIn("foreign text", source_text(result))

    def test_no_readable_text_is_reported_honestly(self) -> None:
        backend = FakeBackend(
            [make_app(42, [make_frame("Empty Window", [], pid=42)])]
        )

        result = capture_fixture(make_window(42, "Empty Window"), backend)

        self.assertEqual(result["sources"], [])
        self.assertTrue(
            any("no readable" in warning for warning in result["warnings"])
        )

    def test_missing_title_and_pid_are_reported(self) -> None:
        backend = FakeBackend(
            [make_app(42, [make_frame("Window", [], pid=42)])]
        )

        no_title = capture_fixture(make_window(42, ""), backend)
        no_pid = capture_fixture(make_window(0, "Window"), backend)

        self.assertEqual(no_title["sources"], [])
        self.assertTrue(any("title" in warning for warning in no_title["warnings"]))
        self.assertEqual(no_pid["sources"], [])
        self.assertTrue(any("pid" in warning for warning in no_pid["warnings"]))

    def test_context_without_window_is_reported(self) -> None:
        backend = FakeBackend([])
        result = capture_text.run_capture(
            {"kind": "workspace"}, backend=backend, enable_tmux=False
        )
        self.assertEqual(result["sources"], [])
        self.assertTrue(
            any("no anchored window" in warning for warning in result["warnings"])
        )

    def test_non_object_context_is_reported(self) -> None:
        result = capture_text.run_capture("nope", backend=FakeBackend([]))
        self.assertEqual(result["sources"], [])
        self.assertTrue(
            any("invalid context" in warning for warning in result["warnings"])
        )


class CaptureOptionsTests(unittest.TestCase):
    def test_options_can_disable_both_sources(self) -> None:
        frame = make_frame(
            "Options Window",
            [FakeNode(role="label", text="should not be read")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])
        context = make_window(42, "Options Window")
        context["capture_options"] = {"accessibility": False, "terminal": False}

        with mock.patch.object(capture_text, "capture_tmux") as tmux:
            result = capture_text.run_capture(context, backend=backend)

        self.assertEqual(result["sources"], [])
        self.assertNotIn("should not be read", source_text(result))
        tmux.assert_not_called()

    def test_options_disable_only_terminal_capture(self) -> None:
        frame = make_frame(
            "Options Window",
            [FakeNode(role="label", text="accessibility still works")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])
        context = make_window(42, "Options Window")
        context["capture_options"] = {"terminal": False}

        with mock.patch.object(capture_text, "capture_tmux") as tmux:
            result = capture_text.run_capture(context, backend=backend)

        self.assertIn("accessibility still works", source_text(result))
        tmux.assert_not_called()

    def test_options_max_text_chars_is_honored(self) -> None:
        frame = make_frame(
            "Options Window",
            [FakeNode(role="text", text="abcdefghijklmnopqrstuvwxyz")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])
        context = make_window(42, "Options Window")
        context["capture_options"] = {"max_text_chars": 10}

        result = capture_text.run_capture(
            context, backend=backend, enable_tmux=False
        )

        self.assertEqual(result["sources"][0]["text"], "abcdefghij")
        self.assertTrue(
            any("truncated" in warning for warning in result["warnings"])
        )

    def test_options_invalid_values_fall_back_and_clamp(self) -> None:
        options, warnings = capture_text.parse_capture_options(
            {
                "accessibility": "yes",
                "terminal": 1,
                "max_text_chars": capture_text.HARD_MAX_TEXT_CHARS + 1,
                "max_nodes": capture_text.HARD_MAX_NODES + 1,
                "max_depth": capture_text.HARD_MAX_DEPTH + 1,
                "budget_ms": capture_text.HARD_MAX_BUDGET_MS + 1,
            }
        )
        self.assertTrue(options.accessibility)
        self.assertTrue(options.terminal)
        self.assertEqual(options.max_text_chars, capture_text.HARD_MAX_TEXT_CHARS)
        self.assertEqual(options.max_nodes, capture_text.HARD_MAX_NODES)
        self.assertEqual(options.max_depth, capture_text.HARD_MAX_DEPTH)
        self.assertEqual(options.budget_ms, capture_text.HARD_MAX_BUDGET_MS)
        self.assertTrue(any("not a boolean" in warning for warning in warnings))
        self.assertTrue(any("clamped" in warning for warning in warnings))

        options, warnings = capture_text.parse_capture_options(
            {"max_text_chars": 0, "max_nodes": "lots", "max_depth": -1}
        )
        self.assertIsNone(options.max_text_chars)
        self.assertIsNone(options.max_nodes)
        self.assertIsNone(options.max_depth)
        self.assertTrue(any("must be positive" in warning for warning in warnings))
        self.assertTrue(any("not an integer" in warning for warning in warnings))

        options, warnings = capture_text.parse_capture_options(
            {"max_text_chars": "lots"}
        )
        self.assertIsNone(options.max_text_chars)
        self.assertTrue(any("not an integer" in warning for warning in warnings))

        options, warnings = capture_text.parse_capture_options("not an object")
        self.assertEqual(options, capture_text.CaptureOptions())
        self.assertTrue(any("not an object" in warning for warning in warnings))

    def test_deep_options_change_nodes_depth_and_budget(self) -> None:
        frame = make_frame(
            "Deep Options Window",
            [FakeNode(role="text", text="deep text")],
            pid=42,
        )
        backend = FakeBackend([make_app(42, [frame])])
        context = make_window(42, "Deep Options Window")
        context["capture_options"] = {
            "max_text_chars": 100,
            "max_nodes": 10,
            "max_depth": 3,
            "budget_ms": 900,
        }

        with mock.patch.object(capture_text, "capture_tmux") as tmux:
            result = capture_text.run_capture(
                context, backend=backend, enable_tmux=False
            )

        self.assertEqual(source_text(result), "deep text")
        tmux.assert_not_called()

        limits = capture_text._apply_capture_options(
            capture_text.DEFAULT_LIMITS,
            capture_text.CaptureOptions(
                max_nodes=10, max_depth=3, budget_ms=900
            ),
        )
        self.assertEqual(limits.max_nodes, 10)
        self.assertEqual(limits.max_depth, 3)
        self.assertAlmostEqual(limits.total_budget_s, 0.9)
        self.assertAlmostEqual(limits.accessibility_budget_s, 0.9)

    def test_absent_options_keep_the_defaults(self) -> None:
        options, warnings = capture_text.parse_capture_options(None)
        self.assertEqual(options, capture_text.CaptureOptions())
        self.assertEqual(warnings, [])


# ---------------------------------------------------------------------------
# tmux ancestry unit tests
# ---------------------------------------------------------------------------


class TmuxAncestryTests(unittest.TestCase):
    def test_descendant_and_self_match(self) -> None:
        parents = {100: 50, 50: 10, 10: 1, 1: 0}
        ppid = parents.get
        self.assertTrue(capture_text.is_descendant(100, 50, ppid))
        self.assertTrue(capture_text.is_descendant(100, 10, ppid))
        self.assertTrue(capture_text.is_descendant(50, 50, ppid))
        self.assertFalse(capture_text.is_descendant(10, 50, ppid))
        self.assertFalse(capture_text.is_descendant(100, 999, ppid))

    def test_cycle_terminates(self) -> None:
        ppid = {7: 8, 8: 7}.get
        self.assertFalse(capture_text.is_descendant(7, 99, ppid))

    def test_unreadable_parent_stops_cleanly(self) -> None:
        self.assertFalse(capture_text.is_descendant(7, 99, lambda _pid: None))

    def test_client_selection_uses_ancestry_and_dedupes(self) -> None:
        parents = {300: 200, 200: 100, 100: 1}
        clients = [
            capture_text.TmuxClient(300, "/dev/pts/3", "session-a"),
            capture_text.TmuxClient(300, "/dev/pts/3", "session-a"),
            capture_text.TmuxClient(400, "/dev/pts/4", "session-b"),
        ]

        matches = capture_text.match_tmux_clients(clients, 100, parents.get)

        self.assertEqual(
            matches, [capture_text.TmuxClient(300, "/dev/pts/3", "session-a")]
        )

    def test_two_clients_under_one_anchor_are_kept_for_ambiguity(self) -> None:
        parents = {300: 100, 301: 100, 100: 1}
        clients = [
            capture_text.TmuxClient(300, "/dev/pts/3", "session-a"),
            capture_text.TmuxClient(301, "/dev/pts/4", "session-a"),
        ]

        matches = capture_text.match_tmux_clients(clients, 100, parents.get)

        self.assertEqual(len(matches), 2)


# ---------------------------------------------------------------------------
# Live tmux test (private server, disposable client)
# ---------------------------------------------------------------------------


@unittest.skipUnless(shutil.which("tmux"), "tmux is not installed")
class LiveTmuxTests(unittest.TestCase):
    def test_captures_the_anchored_clients_own_pane(self) -> None:
        tmux = shutil.which("tmux")
        assert tmux is not None
        name = f"drops-test-{os.getpid()}-{time.time_ns() % 100000}"
        alpha = f"scrollback-alpha-{os.getpid()}"
        beta = f"scrollback-beta-{os.getpid()}"
        master = slave = None
        client: Optional[subprocess.Popen[bytes]] = None
        old_tmux = os.environ.get("TMUX")
        old_tmux_pane = os.environ.get("TMUX_PANE")

        try:
            subprocess.run(
                [
                    tmux,
                    "-L",
                    name,
                    "new-session",
                    "-d",
                    "-s",
                    "probe",
                    f"printf '%s\\n' '{alpha}' '{beta}'; sleep 300",
                ],
                check=True,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=10,
            )

            master, slave = pty.openpty()
            environment = dict(os.environ, TERM="xterm-256color")
            client = subprocess.Popen(
                [tmux, "-L", name, "attach-session", "-t", "probe"],
                stdin=slave,
                stdout=slave,
                stderr=slave,
                env=environment,
                start_new_session=True,
            )
            os.close(slave)
            slave = None

            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                listing = subprocess.run(
                    [
                        tmux,
                        "-L",
                        name,
                        "list-clients",
                        "-F",
                        "#{client_pid}\t#{client_tty}",
                    ],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if listing.returncode == 0 and listing.stdout.strip():
                    break
                time.sleep(0.1)
            else:
                self.fail("private tmux client did not attach in time")

            # The client may attach before the session command has drawn its
            # output.  Wait for the pane (not the client list) to be ready.
            pane_deadline = time.monotonic() + 5
            while time.monotonic() < pane_deadline:
                pane = subprocess.run(
                    [tmux, "-L", name, "capture-pane", "-p", "-t", "probe"],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if pane.returncode == 0 and alpha in pane.stdout and beta in pane.stdout:
                    break
                time.sleep(0.1)
            else:
                self.fail("private tmux pane did not produce its test output in time")

            socket_path = subprocess.run(
                [tmux, "-L", name, "display-message", "-p", "#{socket_path}"],
                capture_output=True,
                text=True,
                check=True,
                timeout=5,
            ).stdout.strip()
            self.assertTrue(socket_path)
            os.environ["TMUX"] = f"{socket_path},0,0"
            os.environ.pop("TMUX_PANE", None)

            source, warnings = capture_text.capture_tmux(
                {"pid": os.getpid()},
                capture_text.DEFAULT_LIMITS,
                time.monotonic() + 5,
            )

            self.assertIsNotNone(source, warnings)
            assert source is not None
            self.assertEqual(source["kind"], "tmux")
            self.assertEqual(source["scope"], "terminal-scrollback")
            self.assertIn(alpha, source["text"])
            self.assertIn(beta, source["text"])
            self.assertTrue(source["pane_id"].startswith("%"))
            self.assertEqual(source["session"], "probe")
        finally:
            if old_tmux is None:
                os.environ.pop("TMUX", None)
            else:
                os.environ["TMUX"] = old_tmux
            if old_tmux_pane is None:
                os.environ.pop("TMUX_PANE", None)
            else:
                os.environ["TMUX_PANE"] = old_tmux_pane

            if client is not None:
                try:
                    os.killpg(client.pid, signal.SIGTERM)
                except OSError:
                    try:
                        client.terminate()
                    except OSError:
                        pass
                try:
                    client.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(client.pid, signal.SIGKILL)
                    except OSError:
                        pass
                    client.wait(timeout=3)
            subprocess.run(
                [tmux, "-L", name, "kill-server"],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=5,
            )
            if master is not None:
                try:
                    os.close(master)
                except OSError:
                    pass


# ---------------------------------------------------------------------------
# Live GTK test (disposable window only)
# ---------------------------------------------------------------------------


def wayland_environment() -> Optional[dict[str, str]]:
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    display = os.environ.get("WAYLAND_DISPLAY")
    if not display:
        sockets = [
            path
            for path in glob.glob(os.path.join(runtime, "wayland-*"))
            if not path.endswith(".lock")
        ]
        if sockets:
            display = os.path.basename(sorted(sockets)[0])
    if not display:
        return None
    environment = dict(os.environ)
    environment["XDG_RUNTIME_DIR"] = runtime
    environment["WAYLAND_DISPLAY"] = display
    environment["GDK_BACKEND"] = "wayland"
    environment.pop("NO_AT_BRIDGE", None)
    environment.setdefault(
        "DBUS_SESSION_BUS_ADDRESS", f"unix:path={runtime}/bus"
    )
    return environment


@unittest.skipUnless(HAVE_ATSPI, f"AT-SPI bindings unavailable: {ATSPI_ERROR}")
@unittest.skipUnless(HAVE_GTK4, "GTK 4 typelib is not available")
class LiveGtkTests(unittest.TestCase):
    def test_disposable_gtk_window_text_and_password_exclusion(self) -> None:
        environment = wayland_environment()
        if environment is None:
            self.skipTest("no Wayland display is available for a live GTK window")

        title = f"Drops live capture {os.getpid()} {time.time_ns()}"
        application_id = f"dev.drops.CaptureTest{os.getpid()}"
        process = subprocess.Popen(
            [
                sys.executable,
                str(Path(__file__).with_name("gtk_probe_app.py")),
                title,
                application_id,
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )

        try:
            deadline = time.monotonic() + 10
            registered = False
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    break
                desktop = Atspi.get_desktop(0)
                for index in range(desktop.get_child_count()):
                    app = desktop.get_child_at_index(index)
                    if app is None or app.get_process_id() != process.pid:
                        continue
                    for frame_index in range(app.get_child_count()):
                        frame = app.get_child_at_index(frame_index)
                        if frame is not None and frame.get_name() == title:
                            registered = True
                            break
                    if registered:
                        break
                if registered:
                    break
                time.sleep(0.15)

            if not registered:
                stdout, stderr = process.communicate(timeout=2)
                self.fail(
                    "disposable GTK window never appeared in the AT-SPI tree; "
                    f"exit={process.returncode} stdout={stdout[-500:]!r} "
                    f"stderr={stderr[-1000:]!r}"
                )

            context = make_window(process.pid, title, cls="python")
            result = capture_text.run_capture(
                context,
                limits=capture_text.Limits(
                    total_budget_s=4.0, accessibility_budget_s=3.5
                ),
                enable_tmux=False,
            )

            self.assertTrue(result["sources"], result["warnings"])
            self.assertEqual(result["sources"][0]["kind"], "accessibility")
            self.assertEqual(result["sources"][0]["scope"], "matched-window")
            text = source_text(result)
            self.assertIn("Drops live accessibility probe 24680", text)
            self.assertIn("editor line one", text)
            self.assertIn("editor line two", text)
            self.assertIn("visible entry text", text)
            self.assertTrue(any(selection["text"] == "editor line one"
                                for selection in result["app_context"]["selections"]), result["app_context"])
            self.assertNotIn("drops-test-secret-9f3a", text)
            self.assertTrue(
                any("password" in warning for warning in result["warnings"]),
                result["warnings"],
            )
        finally:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
            for stream in (process.stdout, process.stderr):
                if stream is not None:
                    stream.close()


# ---------------------------------------------------------------------------
# CLI behaviour
# ---------------------------------------------------------------------------


class CliTests(unittest.TestCase):
    def run_cli(
        self, stdin: str, env: Optional[dict[str, str]] = None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=stdin,
            text=True,
            capture_output=True,
            env=env,
            timeout=15,
        )

    def test_invalid_json_still_emits_one_json_line(self) -> None:
        completed = self.run_cli("this is not json\n")
        self.assertEqual(completed.returncode, 0)
        self.assertEqual(completed.stdout.count("\n"), 1)
        result = json.loads(completed.stdout)
        self.assertEqual(result["sources"], [])
        self.assertTrue(
            any("invalid context JSON" in warning for warning in result["warnings"])
        )

    def test_empty_stdin_is_reported(self) -> None:
        completed = self.run_cli("")
        self.assertEqual(completed.returncode, 0)
        result = json.loads(completed.stdout)
        self.assertEqual(result["sources"], [])
        self.assertTrue(
            any("no context bundle" in warning for warning in result["warnings"])
        )

    def test_runs_the_way_rust_embeds_it_with_python_dash_c(self) -> None:
        # The Rust capture module embeds this file with include_str! and runs
        # it as ``python -B -c <source>``, so __file__ must not be required.
        source = SCRIPT.read_text(encoding="utf-8")
        completed = subprocess.run(
            [sys.executable, "-B", "-c", source],
            input=json.dumps({"kind": "workspace"}) + "\n",
            text=True,
            capture_output=True,
            env=dict(os.environ),
            timeout=15,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result["sources"], [])
        self.assertTrue(
            any("no anchored window" in warning for warning in result["warnings"])
        )

    def test_cli_options_skip_sources_even_when_gi_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "gi.py").write_text(
                "raise ImportError('simulated missing gi')\n", encoding="utf-8"
            )
            environment = dict(os.environ)
            environment["PYTHONPATH"] = directory
            environment.pop("TMUX", None)
            environment.pop("TMUX_PANE", None)
            context = make_window(os.getpid(), "Disabled Window")
            context["capture_options"] = {
                "accessibility": False,
                "terminal": False,
            }
            completed = self.run_cli(json.dumps(context) + "\n", env=environment)

        self.assertEqual(completed.returncode, 0)
        result = json.loads(completed.stdout)
        self.assertEqual(result["sources"], [])
        self.assertFalse(
            any("accessibility unavailable" in warning for warning in result["warnings"])
        )

    def test_missing_gi_degrades_to_a_warning(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "gi.py").write_text(
                "raise ImportError('simulated missing gi')\n", encoding="utf-8"
            )
            environment = dict(os.environ)
            environment["PYTHONPATH"] = directory
            environment.pop("TMUX", None)
            environment.pop("TMUX_PANE", None)
            window = make_window(os.getpid(), "No GI Window")
            completed = self.run_cli(json.dumps(window) + "\n", env=environment)

        self.assertEqual(completed.returncode, 0)
        result = json.loads(completed.stdout)
        self.assertEqual(result["sources"], [])
        self.assertTrue(
            any(
                "accessibility unavailable" in warning
                and "gi" in warning.casefold()
                for warning in result["warnings"]
            ),
            result["warnings"],
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
