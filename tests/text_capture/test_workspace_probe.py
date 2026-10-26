"""Window-specific automatic folder detection without a desktop."""
import pathlib
import shutil
import subprocess
import unittest
from unittest import mock

from test_capture_text import FakeBackend, FakeNode, make_app, make_frame, make_window
import capture_text


class WorkspaceProbeTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("dbus-run-session"), "private D-Bus session unavailable")
    def test_real_file_manager_dbus_api_and_pid_ownership(self):
        # An isolated session bus exercises Gio variants and actual property
        # calls without contacting the user's desktop or any real file manager.
        script = r'''
import os, sys, threading
sys.path.insert(0, sys.argv[1])
import capture_text
from gi.repository import Gio, GLib
bus=Gio.bus_get_sync(Gio.BusType.SESSION, None)
xml='<node><interface name="org.freedesktop.FileManager1"><property name="OpenLocations" type="as" access="read"/></interface></node>'
info=Gio.DBusNodeInfo.new_for_xml(xml).interfaces[0]
registration=bus.register_object('/org/freedesktop/FileManager1', info, None,
    lambda *args: GLib.Variant('as', ['file:///tmp/Reports', 'file:///tmp/Other']), None)
bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'RequestName', GLib.Variant('(su)', ('org.freedesktop.FileManager1', 0)), None, Gio.DBusCallFlags.NONE, 1000, None)
loop=GLib.MainLoop()
thread=threading.Thread(target=loop.run, daemon=True);thread.start()
assert capture_text.file_manager_locations(os.getpid()) == ['file:///tmp/Reports', 'file:///tmp/Other']
assert capture_text.file_manager_locations(os.getpid()+100000) == []
loop.quit();thread.join(timeout=1)
bus.unregister_object(registration)
print('PASS: live FileManager1 properties and PID ownership')
'''
        result=subprocess.run(["dbus-run-session", "--", "/usr/bin/python", "-c", script,
                               str(pathlib.Path(capture_text.__file__).parent)], capture_output=True, text=True, timeout=8)
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)

    def probe(self, nodes=(), *, title="Reports — Files", locations=(), manager=True):
        context = make_window(42, title, "org.gnome.Nautilus")
        context["file_manager"] = manager
        backend = FakeBackend([make_app(42, [make_frame(title, list(nodes), 42)])])
        return capture_text.run_workspace_probe(context, backend=backend, locations=list(locations))

    def location(self, path, **kwargs):
        return FakeNode(name="Location", role="entry", text=path, states={"showing"}, **kwargs)

    def test_live_location_beats_stale_launch_and_other_tabs(self):
        result = self.probe([self.location("/tmp/current folder")], locations=["file:///tmp/stale"])
        self.assertEqual(result["paths"], ["/tmp/current folder"])
        self.assertEqual(result["source"], "window-location")

    def test_hidden_tabs_passwords_and_file_rows_are_ignored(self):
        hidden = self.location("/tmp/hidden"); hidden.states=set()
        protected = self.location("/tmp/protected"); protected.states.add("protected")
        rows = FakeNode(role="table", states={"showing"}, children=[self.location("/tmp/row")] * 2000)
        result=self.probe([self.location("/tmp/current"), hidden, protected, rows])
        self.assertEqual(result["paths"], ["/tmp/current"])

    def test_two_visible_panes_are_both_reported(self):
        self.assertEqual(self.probe([self.location("/tmp/one"), self.location("/tmp/two")])["paths"], ["/tmp/one", "/tmp/two"])

    def test_dbus_paths_are_disambiguated_by_this_window_title(self):
        result=self.probe(locations=["file:///tmp/Reports", "file:///tmp/Other"])
        self.assertEqual(result["paths"], ["/tmp/Reports"])
        self.assertEqual(self.probe(locations=["file:///tmp/Reports", "file:///elsewhere/Reports"])["paths"], ["/elsewhere/Reports", "/tmp/Reports"])
        self.assertEqual(self.probe(title="Trash", locations=["file:///tmp/Reports"])["paths"], [])
        self.assertTrue(self.probe(locations=["file:///tmp/Reports", "sftp://server/Reports"])["unsupported_location"])

    def test_encoded_local_uris_and_remote_rejection(self):
        self.assertEqual(self.probe([self.location("file:///tmp/a%20b%23c")])["paths"], ["/tmp/a b#c"])
        for value in ["file://server/tmp/Reports", "sftp://server/Reports", "trash:///"]:
            result=self.probe([self.location(value)], locations=["file:///tmp/Reports"])
            self.assertEqual(result["paths"], [])
            self.assertTrue(result["unsupported_location"])
        for value in ["file:///tmp/%00bad", "file://[invalid", "relative", "https://example.com"]:
            self.assertIsNone(capture_text.local_path(value))

    def test_other_pid_or_duplicate_title_never_reads_location(self):
        context=make_window(42, "Same"); context["file_manager"]=True
        foreign=FakeBackend([make_app(43, [make_frame("Same", [self.location("/tmp/foreign")], 43)])])
        self.assertEqual(capture_text.run_workspace_probe(context, backend=foreign, locations=[])["paths"], [])
        duplicate=FakeBackend([make_app(42, [make_frame("Same", [self.location("/tmp/one")], 42), make_frame("Same", [self.location("/tmp/two")], 42)])])
        self.assertEqual(capture_text.run_workspace_probe(context, backend=duplicate, locations=[])["paths"], [])

    def test_document_location_for_other_apps_without_reading_body(self):
        document=FakeNode(name="Document title", attrs={"uri":"file:///tmp/report.pdf"}, role="document frame", states={"showing"}, children=[self.location("/tmp/body")])
        result=self.probe([document], manager=False)
        self.assertEqual(result["paths"], ["/tmp/report.pdf"])
        document.attrs={};document.name="/tmp/pretend-folder"
        self.assertEqual(self.probe([document], manager=False)["paths"], [])

    def test_locked_window_never_queries_backend(self):
        backend=mock.Mock()
        result=capture_text.run_workspace_probe({"screen_locked":True,"window":{"pid":42}}, backend=backend)
        self.assertEqual(result["paths"], [])
        backend.roots.assert_not_called()


if __name__ == "__main__":
    unittest.main()
