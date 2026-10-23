#!/usr/bin/python
"""Disposable GTK 4 window used only by tests/text_capture.

Usage: gtk_probe_app.py <exact-window-title> <application-id>

The window exposes a label, a multi-line text view, an ordinary entry, and a
password entry.  It closes itself after 25 seconds as a safety net so a failed
test cannot leave a stray window on the developer's desktop.
"""

import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402


SECRET = "drops-test-secret-9f3a"
LABEL_TEXT = "Drops live accessibility probe 24680"
EDITOR_TEXT = "editor line one\neditor line two\n"


def build_window(app: Gtk.Application, title: str) -> None:
    window = Gtk.ApplicationWindow(application=app, title=title)
    window.set_default_size(640, 420)

    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
    box.append(Gtk.Label(label=LABEL_TEXT))

    view = Gtk.TextView()
    view.get_buffer().set_text(EDITOR_TEXT)
    box.append(view)

    entry = Gtk.Entry()
    entry.set_text("visible entry text")
    box.append(entry)

    password = Gtk.PasswordEntry()
    password.set_text(SECRET)
    box.append(password)

    window.set_child(box)
    window.present()

    GLib.timeout_add_seconds(25, app.quit)


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: gtk_probe_app.py <title> <application-id>", file=sys.stderr)
        return 2
    title, application_id = argv[1], argv[2]
    app = Gtk.Application(application_id=application_id)
    app.connect("activate", lambda application: build_window(application, title))
    return app.run([])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
