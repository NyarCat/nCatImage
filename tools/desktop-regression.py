#!/usr/bin/env python3
"""Isolated X11 key/mouse, drop, mock network, cancellation and privacy checks."""
import os
import subprocess
import time

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk
import pyatspi

from smoke_support import arguments, check_privacy, fixtures, image_command, work_directory


def main():
    args = arguments(__doc__)
    if os.environ.get("NCAT_PRIVATE_TEST_BUS") != "1" or os.environ.get("GDK_BACKEND") != "x11":
        raise SystemExit("Use Xvfb + dbus-run-session with NCAT_PRIVATE_TEST_BUS=1 and GDK_BACKEND=x11; never use your desktop bus")
    Gtk.init()
    context = GLib.MainContext.default()
    work = work_directory("ncat-desktop-")
    inputs = fixtures(work)
    context_deadline = 8

    def pump(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            while context.pending():
                context.iteration(False)
            time.sleep(.005)

    def wait(check, label, seconds=context_deadline):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            pump(.04)
            try:
                result = check()
                if result:
                    return result
            except (subprocess.CalledProcessError, IndexError, RuntimeError):
                pass
        raise AssertionError(label)

    def command(*argv):
        return subprocess.check_output(argv, text=True, stderr=subprocess.DEVNULL).strip()

    def key(name):
        subprocess.run(["xdotool", "key", "--clearmodifiers", name], check=True)
        pump(.13)

    def nodes(node=None):
        if node is None:
            desktop = pyatspi.Registry.getDesktop(0)
            for application in desktop:
                # Ignore the test's own GTK drag source and stale apps during
                # process restarts; only query accessibility on the viewer.
                try:
                    if application and application.name in ["ncat-linux", "ncat-desktop"]:
                        yield from nodes(application)
                except (GLib.Error, RuntimeError):
                    continue
            return
        yield node
        for child in node:
            if child:
                yield from nodes(child)

    def showing(role):
        return [n for n in nodes() if n.getRoleName() == role and n.getState().contains(pyatspi.STATE_SHOWING)]

    def window():
        return wait(lambda: command("xdotool", "search", "--onlyvisible", "--name", "nCatImage").splitlines()[-1], "viewer")

    def activate(win):
        subprocess.run(["xdotool", "windowactivate", "--sync", win], check=True)

    def click(win, x, y):
        # Fixed Ubuntu GTK/Adwaita 1000x800 fixture. GTK 4.14's AT-SPI screen
        # origins can be zero, so use screenshot-verified window coordinates.
        subprocess.run(["xdotool", "mousemove", "--window", win, str(x), str(y), "click", "1"], check=True)
        pump(.13)

    def click_spin(win):
        spin = showing("spin button")[0]
        # Window-relative AT-SPI coordinates avoid GTK's unsupported screen
        # origins and account for client-side decorations and font metrics.
        bounds = spin.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
        click(win, bounds.x + bounds.width // 3, bounds.y + bounds.height // 2)

    def log_snapshot(win, directory):
        activate(win)
        subprocess.run(["xdotool", "mousemove", "--window", win, "380", "320", "click", "3"], check=True)
        # GTK 4.14 exports the popover items in order but may omit their names.
        items = wait(lambda: showing("menu item"), "context menu")
        assert len(items) == 4
        popup = wait(lambda: command("xdotool", "search", "--onlyvisible", "--name",
                                     "^ncat-linux$").splitlines()[-1], "popup surface")
        bounds = items[-1].queryComponent().getExtents(pyatspi.WINDOW_COORDS)
        click(popup, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
        dialog = wait(lambda: next((n for n in nodes() if n.name == "로그 보기"
                                     and n.getRoleName() in ["frame", "dialog"]), None), "log window")
        text_node = wait(lambda: next((n for n in nodes(dialog) if n.getRoleName() == "text"), None),
                         "log text")
        assert not text_node.getState().contains(pyatspi.STATE_EDITABLE)
        text = text_node.queryText().getText(0, -1)
        key("Escape")
        wait(lambda: not any(n.name == "로그 보기" and n.getRoleName() in ["frame", "dialog"]
                             for n in nodes()), "close log window")
        assert not (directory / "log").exists(), "app created a diagnostic log directory"
        check_privacy(text)
        return text

    def launch(label, uri=None):
        directory = work / label
        directory.mkdir()
        argv = [str(uri)] if uri else []
        return subprocess.Popen(image_command(args.image, *argv), cwd=directory), directory

    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    reply = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                          GLib.Variant("(su)", ("org.kde.KIOFuse", 4)), GLib.VariantType.new("(u)"),
                          Gio.DBusCallFlags.NONE, 2000, None)
    assert reply.unpack() == (1,), "KIOFuse already owns this bus; refusing to replace it"
    xml = '<node><interface name="org.kde.KIOFuse.VFS"><method name="mountUrl"><arg type="s" direction="in"/><arg type="s" direction="out"/></method></interface></node>'
    pending = []
    delayed = []

    def on_call(connection, sender, path, interface, method, params, invocation):
        uri = params.unpack()[0]
        assert uri.split(":", 1)[0] in ["smb", "ftp", "sftp"]
        if "/hang" in uri:
            pending.append(invocation)
        elif "/delay" in uri:
            delayed.append(invocation)
        else:
            invocation.return_value(GLib.Variant("(s)", (str(inputs["book"]),)))

    registration = bus.register_object("/org/kde/KIOFuse", Gio.DBusNodeInfo.new_for_xml(xml).interfaces[0], on_call, None, None)
    wm = subprocess.Popen(["openbox"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    app = None
    source = None
    try:
        wait(lambda: "window id #" in command("xprop", "-root", "_NET_SUPPORTING_WM_CHECK"),
             "window manager readiness")
        app, directory = launch("ui", inputs["book"])
        win = window()
        activate(win)
        def page(number):
            wait(lambda: command("xdotool", "getwindowname", win).startswith(f"{number}/3 · "), f"page {number}")
        page(1)
        text = log_snapshot(win, directory)
        assert "backend_ready" in text and "Ready { wayland: false }" in text
        print("PASS context menu log viewer, read-only text, and no log files")
        wait(lambda: showing("spin button"), "accessible controls")
        for press, target in [("Down", 2), ("Up", 1), ("space", 2), ("BackSpace", 1), ("Next", 2), ("Prior", 1), ("Tab", 1), ("Down", 2), ("Up", 1)]:
            key(press)
            page(target)
        key("d")
        for press, target in [("Down", 2), ("Up", 1), ("Left", 2), ("Right", 1)]:
            key(press)
            page(target)
        click_spin(win)
        key("ctrl+a")
        key("2")
        key("Return")
        page(2)
        key("Up")
        page(3)
        click(win, 500, 450)
        key("Up")
        page(2)
        def image_size():
            extent = showing("scroll pane")[0].queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
            return extent.width, extent.height

        normal_size = image_size()
        key("grave")
        wait(lambda: not showing("spin button"), "unpin interface")
        assert image_size() == normal_size, "overlays must not resize the image area"
        key("grave")
        wait(lambda: showing("spin button"), "pin interface")

        def fullscreen():
            return "_NET_WM_STATE_FULLSCREEN" in command("xprop", "-id", win, "_NET_WM_STATE")

        key("F11")
        wait(fullscreen, "fullscreen")
        assert showing("spin button"), "pinned controls remain visible in fullscreen"
        key("grave")
        wait(lambda: not showing("spin button"), "unpin in fullscreen")
        key("F11")
        wait(lambda: not fullscreen(), "windowed restore")
        assert not showing("spin button"), "unpinned controls stay unpinned after fullscreen"
        key("grave")
        wait(lambda: showing("spin button"), "pin again")
        click_spin(win)
        key("Escape")
        wait(lambda: app.poll() is not None, "exit while editing")
        assert app.returncode == 0
        print("PASS keyboard, explicit focus, top UI visibility and fullscreen restoration")

        damaged = work / "damaged-pages"
        damaged.mkdir()
        valid_image = inputs["last"].read_bytes()
        (damaged / "page1.png").write_bytes(valid_image)
        (damaged / "page2.png").write_bytes(valid_image[:24])
        (damaged / "page3.png").write_bytes(valid_image)
        app, directory = launch("damaged-page", damaged)
        win = window()
        activate(win)
        wait(lambda: "1/3 · page1.png · Png" in command("xdotool", "getwindowname", win), "first image")
        key("Down")
        wait(lambda: "2/3 · page2.png · 손상" in command("xdotool", "getwindowname", win), "page error")
        key("Down")
        wait(lambda: "3/3 · page3.png · Png" in command("xdotool", "getwindowname", win), "page after error")
        key("Escape")
        wait(lambda: app.poll() is not None, "exit after page error")
        print("PASS page error unlocks manual navigation")

        source = Gtk.Window(title="NCAT_DRAG_SOURCE")
        source.set_default_size(250, 200)
        source.set_child(Gtk.Label(label="Drag synthetic fixture"))
        drag = Gtk.DragSource()
        drag.set_actions(Gdk.DragAction.COPY)
        payload = None
        drag.connect("prepare", lambda *unused: payload)
        source.add_controller(drag)
        source.present()
        source_id = wait(lambda: command("xdotool", "search", "--onlyvisible", "--name", "^NCAT_DRAG_SOURCE$").splitlines()[-1], "drag source")
        subprocess.run(["xdotool", "windowmove", source_id, "20", "30"], check=True)
        cases = [
            ("uri", "text/uri-list", [inputs["book"].as_uri()], "Displayed"),
            ("list", "list", [inputs["book"].as_uri()], "Displayed"),
            ("folder", "text/uri-list", [inputs["folder"].as_uri()], "Displayed"),
            ("multiple", "text/uri-list", [inputs["book"].as_uri(), inputs["folder"].as_uri()], "MultipleItems"),
            ("remote", "text/uri-list", ["smb://DO-NOT-LOG.invalid/book.cbz"], "Displayed"),
            ("unsupported-uri", "text/uri-list", ["https://DO-NOT-LOG.invalid/book.cbz"], "NonLocal"),
            ("invalid", "text/uri-list", [inputs["bad"].as_uri()], "OpenFailed"),
            ("unsupported-text", "text/plain", ["DO-NOT-LOG secret"], "Entered"),
        ]
        for label, kind, uris, expected in cases:
            if kind == "list":
                payload = Gdk.ContentProvider.new_for_value(Gdk.FileList.new_from_array([Gio.File.new_for_uri(uri) for uri in uris]))
            else:
                payload = Gdk.ContentProvider.new_for_bytes(kind, GLib.Bytes.new(("\r\n".join(uris) + "\r\n").encode()))
            app, directory = launch("drop-" + label)
            win = window()
            subprocess.run(["xdotool", "windowsize", win, "800", "700", "windowmove", win, "400", "200"], check=True)
            pump(.25)
            move = subprocess.Popen(["xdotool", "windowactivate", "--sync", source_id, "mousemove", "100", "120", "mousedown", "1", "sleep", ".2", "mousemove", "115", "135", "sleep", ".2", "mousemove", "270", "180", "sleep", ".2", "mousemove", "500", "350", "sleep", ".3", "mousemove", "750", "550", "sleep", ".4", "mouseup", "1"])
            wait(lambda: move.poll() is not None, "drag input")
            def outcome():
                text = log_snapshot(win, directory)
                return text if expected in text else None

            text = wait(outcome, label + " outcome")
            if expected == "Displayed":
                assert "Received" in text and "OpenQueued" in text
            elif expected == "OpenFailed":
                assert "OpenFailed" in text
            else:
                assert "Displayed" not in text and "OpenQueued" not in text
            activate(win)
            key("Escape")
            wait(lambda: app.poll() is not None, "drop exit")
            print("PASS drag/drop and redaction:", label)
        source.destroy()
        source = None

        for protocol in ["smb", "ftp", "sftp"]:
            app, directory = launch(protocol, f"{protocol}://DO-NOT-LOG.invalid/book.cbz")
            win = window()
            page(1)
            text = log_snapshot(win, directory)
            assert "NetworkResolved" in text and "bridge: KioFuse" in text
            activate(win)
            key("Escape")
            wait(lambda: app.poll() is not None, protocol + " open")
            assert app.returncode == 0
            print("PASS mocked OS bridge:", protocol)

        app, directory = launch("cancel-stale", "smb://DO-NOT-LOG.invalid/delay")
        win = window()
        wait(lambda: delayed, "delayed mount request")
        assert "NetworkResolving" in log_snapshot(win, directory)
        activate(win)
        key("ctrl+l")
        wait(lambda: command("xdotool", "search", "--onlyvisible", "--name", "네트워크 파일 또는 폴더 열기"), "network dialog")
        subprocess.run(["xdotool", "type", "--clearmodifiers", "--delay", "1", inputs["last"].as_uri()], check=True)
        key("Return")
        page(3)
        delayed.pop().return_value(GLib.Variant("(s)", (str(inputs["book"]),)))
        pump(.5)
        page(3)
        text = log_snapshot(win, directory)
        assert "NetworkCancelled" in text and "NetworkResolved" not in text
        activate(win)
        key("Escape")
        wait(lambda: app.poll() is not None, "cancel exit")
        print("PASS network entry, cancellation and stale-result suppression")

        app, directory = launch("close-busy", "sftp://DO-NOT-LOG.invalid/hang")
        win = window()
        assert "NetworkResolving" in log_snapshot(win, directory)
        activate(win)
        key("F11")
        key("Escape")
        wait(lambda: app.poll() is not None, "nonblocking fullscreen exit", seconds=3)
        assert not (directory / "log").exists()
        print("PASS fullscreen exit while remote service has not replied")
    finally:
        if app and app.poll() is None:
            app.terminate()
        if source:
            source.destroy()
        wm.terminate()
        bus.unregister_object(registration)
    print(f"Synthetic inputs and results retained in {work}")


if __name__ == "__main__":
    main()
