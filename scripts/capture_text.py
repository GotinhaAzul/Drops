#!/usr/bin/python
"""Read the text of one anchored window through AT-SPI, plus optional tmux scrollback.

This is a standalone helper for Drops.  It is invoked with ``/usr/bin/python``
(the interpreter that has system PyGObject/AT-SPI), reads one JSON context
bundle from stdin, and writes one JSON result object to stdout::

    {"sources": [{"kind": "accessibility" | "tmux", "text": "...",
                  "scope": "matched-window" | "terminal-scrollback", ...}],
     "warnings": ["..."]}

Contract notes
--------------
* Only the application whose process id equals ``window.pid`` is inspected.
* Only a direct top-level frame of that application whose name matches
  ``window.title`` is traversed.  Exact match is preferred; an unambiguous
  whitespace-normalized match is accepted with a warning.  Anything
  ambiguous or unlocatable yields no accessibility source.
* Password / protected nodes and their subtrees are skipped.
* Traversal is bounded by node count, depth, characters, and a wall-clock
  budget.  The parent process still enforces the hard timeout.
* No clipboard, focus, keyboard, mouse, action, or property writes happen.
  Only read-only AT-SPI queries are issued.
* tmux is optional.  The attached client must be the anchored process or a
  descendant of it; an arbitrary active pane is never captured.  When a
  unique client is found, its own active pane is resolved with
  ``display-message -c <client_tty>`` and captured with ``capture-pane``.

AT-SPI API used (Atspi 2.0, https://docs.gtk.org/atspi2/):
``Atspi.get_desktop(i)``, ``Accessible.get_process_id/get_child_count/
get_child_at_index/get_name/get_role_name/get_attributes/get_state_set``,
and ``Atspi.Text.get_character_count(accessible)`` /
``Atspi.Text.get_text(accessible, start_offset, end_offset)``.  The
deprecated ``Accessible.get_text`` alias is not used: in PyGObject it does
not accept offset arguments.  Text offsets are character offsets.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
import unicodedata
from dataclasses import dataclass, replace
from typing import Any, Callable, Iterable, Optional


# ---------------------------------------------------------------------------
# Limits
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Limits:
    """Hard bounds for one helper invocation.

    The defaults mirror the Rust capture defaults (24 000 characters) and
    stay comfortably inside the 2 500 ms helper timeout.
    """

    max_text_chars: int = 24_000
    max_nodes: int = 4_000
    max_depth: int = 40
    total_budget_s: float = 2.2
    accessibility_budget_s: float = 1.7
    tmux_budget_s: float = 1.0
    max_tmux_lines: int = 2_000


DEFAULT_LIMITS = Limits()

# Match the Rust CaptureConfig hard cap.  The default remains 24 000; this is
# only the upper bound for an explicit capture_options.max_text_chars.
HARD_MAX_TEXT_CHARS = 1_000_000


@dataclass(frozen=True)
class CaptureOptions:
    """Per-request switches injected by the Rust capture module.

    ``capture_options`` is optional; absent fields keep the defaults so older
    context bundles behave exactly as before.
    """

    accessibility: bool = True
    terminal: bool = True
    max_text_chars: Optional[int] = None

# Roles whose accessible name is useful as content when no Text interface is
# exposed.  Buttons and menu chrome are deliberately absent: the helper is for
# document/terminal content, not a UI inventory.
_CONTENT_NAME_ROLES = {
    "label",
    "static",
    "static text",
    "text",
    "paragraph",
    "heading",
    "link",
    "list item",
    "table cell",
    "cell",
    "article",
    "comment",
    "caption",
    "description term",
    "description value",
    "block quote",
    "code",
    "section",
}

_PROTECTED_ROLE_NAMES = {"password text", "protected"}
_PROTECTED_STATE_NAMES = {"password text", "protected"}
_PROTECTED_ATTR_KEYS = {"protected", "password", "is-password", "protected-content"}

_ANSI_RE = re.compile(
    r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\)|[@-_])"
)


# ---------------------------------------------------------------------------
# Small data types
# ---------------------------------------------------------------------------


@dataclass
class Selection:
    """The one top-level frame that belongs to the anchored window."""

    app: Any
    app_name: str
    frame: Any
    frame_name: str
    frame_role: str
    frame_pid: int
    match: str  # "exact-title" or "normalized-title"


@dataclass
class Stats:
    nodes: int = 0
    chars: int = 0
    protected_nodes: int = 0
    errors: int = 0
    timed_out: bool = False
    node_limited: bool = False
    depth_limited: bool = False
    truncated: bool = False


@dataclass(frozen=True)
class TmuxClient:
    pid: int
    tty: str
    session: str
    # Socket path when the client came from a non-default server, else None.
    socket: Optional[str] = None


class _Guard:
    """Count backend query failures instead of aborting the whole capture."""

    def __init__(self) -> None:
        self.errors = 0

    def call(self, fn: Callable[[], Any], default: Any) -> Any:
        try:
            return fn()
        except Exception:
            self.errors += 1
            return default


# ---------------------------------------------------------------------------
# AT-SPI backend
# ---------------------------------------------------------------------------


class AtspiBackend:
    """Thin read-only wrapper around a loaded ``Atspi`` GI module."""

    def __init__(self, atspi: Any) -> None:
        self.atspi = atspi
        self._state_names: dict[int, str] = {}
        try:
            for name in dir(atspi.StateType):
                if not name.isupper():
                    continue
                value = getattr(atspi.StateType, name)
                try:
                    self._state_names[int(value)] = name.casefold().replace("_", " ")
                except (TypeError, ValueError):
                    continue
        except Exception:
            # A partially built state map only weakens password-state
            # detection; roles and attributes still protect the node.
            self._state_names = {}

    def roots(self) -> list[Any]:
        """Return every application root exposed on every desktop."""
        apps: list[Any] = []
        try:
            count = int(self.atspi.get_desktop_count() or 0)
        except Exception:
            count = 0
        desktops = []
        if count > 0:
            for index in range(count):
                try:
                    desktop = self.atspi.get_desktop(index)
                except Exception:
                    continue
                if desktop is not None:
                    desktops.append(desktop)
        else:
            try:
                desktop = self.atspi.get_desktop(0)
            except Exception:
                desktop = None
            if desktop is not None:
                desktops.append(desktop)

        for desktop in desktops:
            try:
                child_count = int(desktop.get_child_count())
            except Exception:
                continue
            for index in range(child_count):
                try:
                    app = desktop.get_child_at_index(index)
                except Exception:
                    continue
                if app is not None:
                    apps.append(app)
        return apps

    def name(self, node: Any) -> str:
        return node.get_name() or ""

    def role(self, node: Any) -> str:
        return (node.get_role_name() or "").strip().casefold()

    def pid(self, node: Any) -> int:
        return int(node.get_process_id() or 0)

    def child_count(self, node: Any) -> int:
        return int(node.get_child_count() or 0)

    def child(self, node: Any, index: int) -> Optional[Any]:
        return node.get_child_at_index(index)

    def states(self, node: Any) -> set[str]:
        try:
            values = node.get_state_set().get_states()
        except Exception:
            return set()
        names: set[str] = set()
        for value in values or []:
            try:
                names.add(self._state_names.get(int(value), ""))
            except (TypeError, ValueError):
                continue
        names.discard("")
        return names

    def attributes(self, node: Any) -> dict[str, str]:
        try:
            attrs = node.get_attributes()
        except Exception:
            return {}
        if attrs is None:
            return {}
        if hasattr(attrs, "items"):
            try:
                return {str(key): str(value) for key, value in attrs.items()}
            except Exception:
                return {}
        # Some GI versions expose the attribute set as a flat array.
        try:
            flat = list(attrs)
        except TypeError:
            return {}
        result: dict[str, str] = {}
        for index in range(0, len(flat) - 1, 2):
            result[str(flat[index])] = str(flat[index + 1])
        return result

    def text(self, node: Any, limit: int) -> Optional[tuple[str, int]]:
        """Return ``(text, total_chars)`` or ``None`` without a Text iface.

        At most ``limit`` characters are requested to keep D-Bus payloads
        bounded; ``total_chars`` still reports the real character count.
        """
        try:
            if not node.is_text():
                return None
        except Exception:
            return None
        try:
            total = int(self.atspi.Text.get_character_count(node) or 0)
        except Exception:
            return None
        if total <= 0:
            return "", 0
        take = max(0, min(total, int(limit)))
        if take <= 0:
            return "", total
        try:
            text = self.atspi.Text.get_text(node, 0, take)
        except Exception:
            return None
        return (text or ""), total


def _load_atspi() -> tuple[Optional[Any], Optional[str]]:
    """Import Atspi lazily so missing GI degrades to a warning."""
    try:
        import gi  # type: ignore
    except Exception as exc:  # noqa: BLE001 - missing capability is reported
        return None, (
            "accessibility unavailable: PyGObject gi is not installed for this "
            f"interpreter ({exc})"
        )
    try:
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi  # type: ignore
    except Exception as exc:  # noqa: BLE001 - missing typelib is reported
        return None, (
            "accessibility unavailable: the Atspi 2.0 GI typelib is not "
            f"available ({exc})"
        )
    return Atspi, None


# ---------------------------------------------------------------------------
# Window selection
# ---------------------------------------------------------------------------


def _norm_title(value: str) -> str:
    """Normalize only whitespace/Unicode compatibility, never content."""
    value = unicodedata.normalize("NFKC", value)
    value = value.replace("\u00a0", " ")
    return " ".join(value.split())


def _coerce_pid(value: Any) -> Optional[int]:
    try:
        pid = int(value)
    except (TypeError, ValueError):
        return None
    return pid if pid > 0 else None


def parse_capture_options(raw: Any) -> tuple[CaptureOptions, list[str]]:
    """Parse the optional bounded ``capture_options`` object.

    Invalid values fall back to defaults instead of failing the capture.  The
    character limit is clamped to [1, HARD_MAX_TEXT_CHARS] to keep a bad
    config from producing an unbounded AT-SPI read.
    """
    warnings: list[str] = []
    if raw is None:
        return CaptureOptions(), warnings
    if not isinstance(raw, dict):
        return CaptureOptions(), [
            "capture_options is not an object; using capture defaults"
        ]

    accessibility = True
    terminal = True
    max_text_chars: Optional[int] = None

    value = raw.get("accessibility")
    if isinstance(value, bool):
        accessibility = value
    elif value is not None:
        warnings.append(
            "capture_options.accessibility is not a boolean; using the default"
        )

    value = raw.get("terminal")
    if isinstance(value, bool):
        terminal = value
    elif value is not None:
        warnings.append(
            "capture_options.terminal is not a boolean; using the default"
        )

    value = raw.get("max_text_chars")
    if value is not None:
        if isinstance(value, bool) or not isinstance(value, int):
            warnings.append(
                "capture_options.max_text_chars is not an integer; using the default"
            )
        elif value <= 0:
            warnings.append(
                "capture_options.max_text_chars must be positive; using the default"
            )
        elif value > HARD_MAX_TEXT_CHARS:
            max_text_chars = HARD_MAX_TEXT_CHARS
            warnings.append(
                "capture_options.max_text_chars was clamped to "
                f"{HARD_MAX_TEXT_CHARS} characters"
            )
        else:
            max_text_chars = value

    return (
        CaptureOptions(
            accessibility=accessibility,
            terminal=terminal,
            max_text_chars=max_text_chars,
        ),
        warnings,
    )


def _apply_capture_options(limits: Limits, options: CaptureOptions) -> Limits:
    if options.max_text_chars is None:
        return limits
    return replace(limits, max_text_chars=options.max_text_chars)


def select_window(
    backend: Any,
    pid: int,
    title: str,
    guard: _Guard,
) -> tuple[Optional[Selection], list[str]]:
    """Find the unique top-level frame for ``pid`` and ``title``.

    PID is a hard gate: an application registered for another process is
    never considered, even if it has a frame with the same title.
    """
    warnings: list[str] = []
    apps = list(backend.roots())
    matching_apps = [
        app
        for app in apps
        if guard.call(lambda app=app: backend.pid(app), -1) == pid
    ]
    if not matching_apps:
        warnings.append(
            f"accessibility application not found for anchored pid {pid} "
            f"({len(apps)} application(s) exposed)"
        )
        return None, warnings

    candidates: list[tuple[Any, str, Any, str, str, int]] = []
    for app in matching_apps:
        app_name = guard.call(lambda app=app: backend.name(app), "")
        child_count = guard.call(lambda app=app: backend.child_count(app), 0)
        for index in range(max(0, child_count)):
            frame = guard.call(
                lambda app=app, index=index: backend.child(app, index), None
            )
            if frame is None:
                continue
            role = guard.call(lambda frame=frame: backend.role(frame), "")
            if role in {"application", "desktop frame"}:
                continue
            frame_name = guard.call(lambda frame=frame: backend.name(frame), "")
            frame_pid = guard.call(lambda frame=frame: backend.pid(frame), 0)
            if frame_pid and frame_pid != pid:
                # Never read a frame that claims another process, even when
                # its title happens to match the anchor.
                continue
            candidates.append((app, app_name, frame, frame_name, role, frame_pid))

    if not candidates:
        warnings.append(
            f"accessibility top-level window not found for pid {pid} "
            f"(title {title!r})"
        )
        return None, warnings

    exact = [candidate for candidate in candidates if candidate[3] == title]
    if len(exact) == 1:
        return _selection(exact[0], "exact-title"), warnings
    if len(exact) > 1:
        warnings.append(
            "accessibility window selection is ambiguous: "
            f"{len(exact)} top-level frames for pid {pid} match title {title!r}"
        )
        return None, warnings

    wanted = _norm_title(title)
    normalized = [
        candidate
        for candidate in candidates
        if wanted and _norm_title(candidate[3]) == wanted
    ]
    if len(normalized) == 1:
        warnings.append(
            "matched top-level window after whitespace normalization; "
            "the exact AT-SPI title differed from the compositor title"
        )
        return _selection(normalized[0], "normalized-title"), warnings
    if len(normalized) > 1:
        warnings.append(
            "accessibility window selection is ambiguous after whitespace "
            f"normalization: {len(normalized)} top-level frames for pid {pid} "
            f"match title {title!r}"
        )
        return None, warnings

    warnings.append(
        f"accessibility top-level window not found for pid {pid} "
        f"(title {title!r}; {len(candidates)} top-level frame(s) seen)"
    )
    return None, warnings


def _selection(candidate: tuple[Any, str, Any, str, str, int], match: str) -> Selection:
    app, app_name, frame, frame_name, role, frame_pid = candidate
    return Selection(
        app=app,
        app_name=app_name,
        frame=frame,
        frame_name=frame_name,
        frame_role=role,
        frame_pid=frame_pid,
        match=match,
    )


# ---------------------------------------------------------------------------
# Protected nodes and text collection
# ---------------------------------------------------------------------------


def _attribute_says_protected(attrs: dict[str, str]) -> bool:
    for key, value in attrs.items():
        name = key.strip().casefold()
        text = value.strip().casefold()
        if name in _PROTECTED_ATTR_KEYS and text in {"1", "true", "yes", "on"}:
            return True
        if name == "text-input-type" and text == "password":
            return True
        if name in {"xml-roles", "role"} and "password" in text:
            return True
    return False


def is_protected(backend: Any, node: Any, guard: _Guard) -> bool:
    role = guard.call(lambda: backend.role(node), "")
    if role in _PROTECTED_ROLE_NAMES or "password" in role:
        return True
    states = guard.call(lambda: backend.states(node), set())
    if states & _PROTECTED_STATE_NAMES:
        return True
    attrs = guard.call(lambda: backend.attributes(node), {})
    return _attribute_says_protected(attrs)


def _clean_block(text: str) -> str:
    """Normalize line endings and trim blank edges, preserving indentation."""
    text = text.replace("\x00", "").replace("\r\n", "\n").replace("\r", "\n")
    text = _ANSI_RE.sub("", text)
    lines = [line.rstrip() for line in text.split("\n")]
    while lines and not lines[0].strip():
        lines.pop(0)
    while lines and not lines[-1].strip():
        lines.pop()
    cleaned: list[str] = []
    blank_run = 0
    for line in lines:
        if line.strip():
            blank_run = 0
            cleaned.append(line)
        else:
            blank_run += 1
            if blank_run <= 2:
                cleaned.append("")
    return "\n".join(cleaned)


def _canon(text: str) -> str:
    return " ".join(text.split())


def collect_text(
    backend: Any,
    frame: Any,
    limits: Limits,
    deadline: float,
    guard: _Guard,
) -> tuple[str, Stats]:
    """Collect bounded document text from one frame, depth first."""
    stats = Stats()
    blocks: list[str] = []

    def visit(node: Any, depth: int, ancestors: list[str]) -> None:
        if stats.truncated:
            return
        if time.monotonic() >= deadline:
            stats.timed_out = True
            return
        stats.nodes += 1
        if stats.nodes > limits.max_nodes:
            stats.node_limited = True
            return

        if is_protected(backend, node, guard):
            stats.protected_nodes += 1
            return

        role = guard.call(lambda: backend.role(node), "")
        remaining = limits.max_text_chars - stats.chars
        if remaining <= 0:
            stats.truncated = True
            return

        info = guard.call(
            lambda: backend.text(node, min(remaining + 1, limits.max_text_chars + 1)),
            None,
        )
        block = ""
        if info is not None:
            raw, total = info
            if total > len(raw):
                stats.truncated = True
            if len(raw) > remaining:
                raw = raw[:remaining]
                stats.truncated = True
            block = _clean_block(raw)

        if not block and role in _CONTENT_NAME_ROLES:
            name = guard.call(lambda: backend.name(node), "")
            block = _clean_block(name)

        if block:
            canonical = _canon(block)
            if canonical and not any(canonical in ancestor for ancestor in ancestors):
                if len(block) > remaining:
                    block = block[:remaining]
                    stats.truncated = True
                blocks.append(block)
                stats.chars += len(block) + 1
                ancestors = ancestors + [canonical]

        if stats.truncated or stats.timed_out:
            return

        if depth >= limits.max_depth:
            child_count = guard.call(lambda: backend.child_count(node), 0)
            if child_count > 0:
                stats.depth_limited = True
            return

        child_count = guard.call(lambda: backend.child_count(node), 0)
        for index in range(max(0, child_count)):
            if stats.truncated or stats.timed_out or stats.node_limited:
                return
            child = guard.call(
                lambda node=node, index=index: backend.child(node, index), None
            )
            if child is not None:
                visit(child, depth + 1, ancestors)

    visit(frame, 0, [])
    text = "\n".join(blocks)
    if len(text) > limits.max_text_chars:
        text = text[: limits.max_text_chars]
        stats.truncated = True
    stats.errors = guard.errors
    return text, stats


def capture_accessibility(
    backend: Any,
    window: dict[str, Any],
    limits: Limits,
    deadline: float,
) -> tuple[Optional[dict[str, Any]], list[str]]:
    """Capture accessibility text for the anchored window only."""
    warnings: list[str] = []
    pid = _coerce_pid(window.get("pid"))
    title = window.get("title")
    if pid is None:
        return None, ["accessibility unavailable: anchored window has no usable pid"]
    if not isinstance(title, str) or not title:
        return None, [
            "accessibility unavailable: anchored window has no title to match"
        ]

    guard = _Guard()
    selection, selection_warnings = select_window(backend, pid, title, guard)
    warnings.extend(selection_warnings)
    if guard.errors:
        warnings.append(
            f"accessibility tree queries failed for {guard.errors} node(s)"
        )
    if selection is None:
        return None, warnings

    guard = _Guard()
    text, stats = collect_text(backend, selection.frame, limits, deadline, guard)
    if stats.timed_out:
        warnings.append(
            "accessibility traversal reached its time budget; text is partial"
        )
    if stats.node_limited:
        warnings.append(
            f"accessibility traversal stopped at the {limits.max_nodes}-node limit; "
            "text is partial"
        )
    if stats.depth_limited:
        warnings.append(
            f"accessibility traversal stopped at the {limits.max_depth}-level depth "
            "limit; text is partial"
        )
    if stats.truncated:
        warnings.append(
            f"accessibility text truncated at {limits.max_text_chars} characters"
        )
    if stats.protected_nodes:
        warnings.append(
            f"skipped {stats.protected_nodes} password/protected accessibility "
            "node(s)"
        )
    if stats.errors:
        warnings.append(
            f"skipped {stats.errors} accessibility node(s) that returned errors"
        )

    if not text.strip():
        warnings.append(
            f"matched window {title!r} exposed no readable accessibility text"
        )
        return None, warnings

    source = {
        "kind": "accessibility",
        "scope": "matched-window",
        "text": text,
        "application": {"pid": pid, "name": selection.app_name},
        "frame": {
            "name": selection.frame_name,
            "role": selection.frame_role,
            "pid": selection.frame_pid or pid,
        },
        "match": selection.match,
        "nodes_visited": stats.nodes,
    }
    return source, warnings


# ---------------------------------------------------------------------------
# tmux (optional)
# ---------------------------------------------------------------------------


def _read_ppid(pid: int) -> Optional[int]:
    try:
        with open(f"/proc/{pid}/stat", "r", encoding="ascii", errors="replace") as handle:
            data = handle.read()
    except OSError:
        return None
    close = data.rfind(")")
    if close < 0:
        return None
    fields = data[close + 2 :].split()
    if len(fields) < 2:
        return None
    try:
        return int(fields[1])
    except ValueError:
        return None


def is_descendant(
    pid: int,
    ancestor_pid: int,
    ppid_of: Callable[[int], Optional[int]] = _read_ppid,
) -> bool:
    """True when ``pid`` is ``ancestor_pid`` or below it in the process tree."""
    current: Optional[int] = pid
    seen: set[int] = set()
    while current and current > 0 and current not in seen:
        if current == ancestor_pid:
            return True
        seen.add(current)
        current = ppid_of(current)
    return False


def match_tmux_clients(
    clients: Iterable[TmuxClient],
    anchored_pid: int,
    ppid_of: Callable[[int], Optional[int]] = _read_ppid,
) -> list[TmuxClient]:
    """Clients attached under (or by) the anchored process."""
    matches: list[TmuxClient] = []
    seen: set[tuple[int, str]] = set()
    for client in clients:
        key = (client.pid, client.tty)
        if key in seen:
            continue
        seen.add(key)
        if is_descendant(client.pid, anchored_pid, ppid_of):
            matches.append(client)
    return matches


def _run_tmux(
    args: list[str], timeout: float, socket: Optional[str] = None
) -> subprocess.CompletedProcess[str]:
    executable = shutil.which("tmux") or "tmux"
    environment = os.environ.copy()
    environment.pop("TMUX_PANE", None)
    if socket:
        environment["TMUX"] = f"{socket},0,0"
    else:
        # Force the default server instead of an inherited socket.
        environment.pop("TMUX", None)
    return subprocess.run(
        [executable, *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        errors="replace",
        timeout=max(0.05, timeout),
        env=environment,
        check=False,
    )


def _tmux_sockets() -> list[Optional[str]]:
    """Discover the default server and any inherited non-default server."""
    sockets: list[Optional[str]] = [None]
    raw = os.environ.get("TMUX", "").strip()
    if raw:
        path = raw.split(",", 1)[0].strip()
        if path:
            sockets.append(path)
    return sockets


def _parse_tmux_clients(
    stdout: str, socket: Optional[str] = None
) -> list[TmuxClient]:
    clients: list[TmuxClient] = []
    for line in stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        try:
            pid = int(parts[0])
        except ValueError:
            continue
        tty = parts[1].strip()
        session = parts[2].strip()
        if pid > 0 and tty:
            clients.append(
                TmuxClient(pid=pid, tty=tty, session=session, socket=socket)
            )
    return clients


def _sanitize_terminal(text: str) -> str:
    text = text.replace("\x00", "").replace("\r\n", "\n").replace("\r", "\n")
    text = _ANSI_RE.sub("", text)
    text = "".join(
        char for char in text if char == "\n" or char == "\t" or ord(char) >= 32
    )
    lines = [line.rstrip() for line in text.split("\n")]
    while lines and not lines[-1]:
        lines.pop()
    while lines and not lines[0]:
        lines.pop(0)
    return "\n".join(lines)


def capture_tmux(
    window: dict[str, Any],
    limits: Limits,
    deadline: float,
) -> tuple[Optional[dict[str, Any]], list[str]]:
    """Capture the matched tmux client's own active pane, if exactly one."""
    warnings: list[str] = []
    pid = _coerce_pid(window.get("pid"))
    if pid is None:
        return None, warnings
    if shutil.which("tmux") is None:
        return None, warnings

    now = time.monotonic()
    remaining = min(limits.tmux_budget_s, deadline - now)
    if remaining <= 0.05:
        return None, warnings

    # Query the default server and an inherited socket (if any).  The daemon
    # may itself have been launched from tmux, which must not hide the server
    # of the anchored terminal.
    clients: list[TmuxClient] = []
    seen: set[tuple[int, str, str, Optional[str]]] = set()
    for socket in _tmux_sockets():
        remaining = min(limits.tmux_budget_s, deadline - time.monotonic())
        if remaining <= 0.05:
            break
        try:
            listing = _run_tmux(
                [
                    "list-clients",
                    "-F",
                    "#{client_pid}\t#{client_tty}\t#{client_session}",
                ],
                remaining,
                socket,
            )
        except (OSError, subprocess.SubprocessError):
            continue
        if listing.returncode != 0:
            # No server, no clients, or an inaccessible socket: tmux is optional.
            continue
        for client in _parse_tmux_clients(listing.stdout, socket):
            key = (client.pid, client.tty, client.session, client.socket)
            if key not in seen:
                seen.add(key)
                clients.append(client)

    matches = match_tmux_clients(clients, pid)
    if not matches:
        return None, warnings
    if len(matches) > 1:
        warnings.append(
            "tmux client selection is ambiguous under anchored pid "
            f"{pid}: {len(matches)} attached clients match; scrollback not captured"
        )
        return None, warnings

    client = matches[0]
    remaining = min(limits.tmux_budget_s, deadline - time.monotonic())
    if remaining <= 0.05:
        warnings.append(
            f"tmux capture skipped for client {client.pid}: time budget exhausted"
        )
        return None, warnings

    try:
        pane_result = _run_tmux(
            [
                "display-message",
                "-p",
                "-c",
                client.tty,
                "#{pane_id}\t#{session_name}\t#{window_index}\t#{pane_index}",
            ],
            remaining,
            client.socket,
        )
    except (OSError, subprocess.SubprocessError):
        warnings.append(f"tmux pane lookup failed for client {client.pid}")
        return None, warnings
    if pane_result.returncode != 0:
        detail = (pane_result.stderr or "").strip().splitlines()
        warnings.append(
            f"tmux pane lookup failed for client {client.pid}"
            + (f": {detail[0]}" if detail else "")
        )
        return None, warnings

    pane_line = next(
        (line for line in pane_result.stdout.splitlines() if line.strip()), ""
    )
    parts = pane_line.split("\t")
    if len(parts) < 4 or not re.fullmatch(r"%\d+", parts[0].strip()):
        warnings.append(
            f"tmux client {client.pid} did not resolve to a usable pane"
        )
        return None, warnings
    pane_id, session, window_index, pane_index = (part.strip() for part in parts[:4])

    remaining = min(limits.tmux_budget_s, deadline - time.monotonic())
    if remaining <= 0.05:
        warnings.append(
            f"tmux capture skipped for pane {pane_id}: time budget exhausted"
        )
        return None, warnings

    try:
        capture = _run_tmux(
            [
                "capture-pane",
                "-p",
                "-J",
                "-S",
                f"-{limits.max_tmux_lines}",
                "-t",
                pane_id,
            ],
            remaining,
            client.socket,
        )
    except (OSError, subprocess.SubprocessError):
        warnings.append(f"tmux capture failed for pane {pane_id}")
        return None, warnings
    if capture.returncode != 0:
        detail = (capture.stderr or "").strip().splitlines()
        warnings.append(
            f"tmux capture failed for pane {pane_id}"
            + (f": {detail[0]}" if detail else "")
        )
        return None, warnings

    text = _sanitize_terminal(capture.stdout)
    if not text:
        warnings.append(f"tmux pane {pane_id} contained no readable text")
        return None, warnings

    if len(text) > limits.max_text_chars:
        text = text[: limits.max_text_chars]
        warnings.append(
            f"tmux scrollback truncated at {limits.max_text_chars} characters"
        )

    source = {
        "kind": "tmux",
        "scope": "terminal-scrollback",
        "text": text,
        "client_pid": client.pid,
        "client_tty": client.tty,
        "session": session or client.session,
        "window_index": window_index,
        "pane_index": pane_index,
        "pane_id": pane_id,
        "lines": text.count("\n") + 1,
    }
    return source, warnings


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


def _dedupe(items: Iterable[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for item in items:
        if item and item not in seen:
            seen.add(item)
            result.append(item)
    return result


def run_capture(
    context: Any,
    *,
    backend: Any = None,
    limits: Limits = DEFAULT_LIMITS,
    enable_tmux: bool = True,
    start: Optional[float] = None,
) -> dict[str, Any]:
    """Run the capture pipeline for one context bundle.

    ``backend`` is injectable for fixture tests; production leaves it ``None``
    and the Atspi GI module is loaded lazily.
    """
    started = time.monotonic() if start is None else start
    deadline = started + limits.total_budget_s
    warnings: list[str] = []
    sources: list[dict[str, Any]] = []

    if not isinstance(context, dict):
        return {
            "sources": [],
            "warnings": ["invalid context: expected a JSON object"],
        }

    window = context.get("window")
    if not isinstance(window, dict):
        return {
            "sources": [],
            "warnings": [
                "context has no anchored window; accessibility and tmux capture "
                "were skipped"
            ],
        }

    options, option_warnings = parse_capture_options(
        context.get("capture_options")
    )
    warnings.extend(option_warnings)
    limits = _apply_capture_options(limits, options)

    if options.accessibility and backend is None:
        atspi, load_warning = _load_atspi()
        if load_warning:
            warnings.append(load_warning)
        if atspi is not None:
            backend = AtspiBackend(atspi)

    if options.accessibility and backend is not None:
        accessibility_deadline = min(
            started + limits.accessibility_budget_s, deadline
        )
        try:
            source, source_warnings = capture_accessibility(
                backend, window, limits, accessibility_deadline
            )
        except Exception as exc:  # noqa: BLE001 - never fail the chat turn
            source = None
            source_warnings = [
                f"accessibility capture failed: {type(exc).__name__}: {exc}"
            ]
        warnings.extend(source_warnings)
        if source is not None:
            sources.append(source)

    if enable_tmux and options.terminal:
        try:
            tmux_source, tmux_warnings = capture_tmux(window, limits, deadline)
        except Exception as exc:  # noqa: BLE001 - tmux is optional
            tmux_source = None
            tmux_warnings = [f"tmux capture failed: {type(exc).__name__}: {exc}"]
        warnings.extend(tmux_warnings)
        if tmux_source is not None:
            # The terminal's accessibility text is normally the visible suffix
            # of its scrollback.  Do not send the same lines twice.
            accessibility = [
                source for source in sources if source.get("kind") == "accessibility"
            ]
            if accessibility and accessibility[0].get("text"):
                acc_text = _canon(accessibility[0]["text"])
                tmux_text = _canon(tmux_source["text"])
                if acc_text and acc_text in tmux_text:
                    sources = [
                        source for source in sources if source is not accessibility[0]
                    ]
                    tmux_source["supersedes_accessibility"] = True
            sources.append(tmux_source)

    return {"sources": sources, "warnings": _dedupe(warnings)}


def _emit(result: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
    sys.stdout.write("\n")
    sys.stdout.flush()


def main(argv: Optional[list[str]] = None) -> int:
    """Read one JSON line from stdin and emit one JSON line."""
    del argv  # no command-line options: the stdin contract is the interface
    raw = sys.stdin.readline()
    if not raw.strip():
        _emit({"sources": [], "warnings": ["no context bundle on stdin"]})
        return 0
    try:
        context = json.loads(raw)
    except Exception as exc:  # noqa: BLE001 - report as a warning
        _emit(
            {
                "sources": [],
                "warnings": [f"invalid context JSON: {type(exc).__name__}: {exc}"],
            }
        )
        return 0

    try:
        result = run_capture(context, limits=DEFAULT_LIMITS)
    except Exception as exc:  # noqa: BLE001 - always emit parseable output
        result = {
            "sources": [],
            "warnings": [f"capture helper failed: {type(exc).__name__}: {exc}"],
        }
    _emit(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
