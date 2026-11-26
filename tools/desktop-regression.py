#!/usr/bin/env python3
"""Isolated X11 key/mouse, drop, mock network, cancellation and privacy checks."""
import json
import os
import subprocess
import time

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, Gio, GLib, Gtk
import pyatspi

from smoke_support import arguments, check_privacy, fixtures, image_command, solid_png, work_directory


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

    def page_field():
        return next((node for node in showing("text")
                     if node.getState().contains(pyatspi.STATE_EDITABLE)), None)

    def click_page_field(win):
        # Window-relative AT-SPI coordinates avoid GTK's unsupported screen
        # origins and account for client-side decorations and font metrics.
        field = wait(page_field, "page number field")
        bounds = field.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
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

    def launch(label, uri=None, state_home=None, data_home=None):
        directory = work / label
        directory.mkdir()
        argv = [str(uri)] if uri else []
        env = {**os.environ, "XDG_STATE_HOME": str(state_home or directory / "state")}
        if data_home is not None:
            env["XDG_DATA_HOME"] = str(data_home)
        return subprocess.Popen(image_command(args.image, *argv), cwd=directory, env=env), directory

    def dimensions(node):
        area = node.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
        return area.x, area.y, area.width, area.height

    def client_size(win):
        geometry = dict(line.split("=", 1) for line in command(
            "xdotool", "getwindowgeometry", "--shell", win
        ).splitlines())
        return int(geometry["WIDTH"]), int(geometry["HEIGHT"])

    def zoom_label(text):
        return next((node for node in showing("label") if node.name == text), None)

    def copy_button():
        return next((node for node in showing("push button") if node.name == "메타데이터 복사"), None)

    def info_button():
        return next((node for node in showing("push button")
                     if node.name in ["ⓘ", "메타데이터 열기", "메타데이터 닫기"]), None)

    def delete_dialog():
        return next((node for node in nodes() if node.name == "삭제하시겠습니까?"
                     and node.getRoleName() in ["frame", "dialog"]), None)

    def click_accessible(name, win, role):
        node = wait(lambda: next((node for node in showing(role) if node.name == name), None), name)
        x, y, w, h = dimensions(node)
        click(win, x + w // 2, y + h // 2)

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
        state_home = work / "window-state"
        state_file = state_home / "ncatimage/window-state.json"
        state_file.parent.mkdir(parents=True)
        state_file.write_text('{"width":800,"height":540}')
        size_fixtures = work / "window-size-fixtures"
        size_fixtures.mkdir()
        original = size_fixtures / "page1.png"
        original.write_bytes(solid_png(930, 400, (60, 130, 180), "Copy <literal>\nSecond line"))
        (size_fixtures / "page2.png").write_bytes(solid_png(1150, 700, (60, 180, 130)))
        app, directory = launch("window-size", original, state_home)
        win = window()
        activate(win)
        wait(lambda: "930×400" in command("xdotool", "getwindowname", win), "first native page")
        wait(lambda: client_size(win) == (930, 540), "startup grows only the required width")
        wait(lambda: showing("image") and dimensions(showing("image")[0]) == (0, 70, 930, 400),
             "center the page in the remaining vertical space")
        badge = wait(lambda: zoom_label("100%\n930 × 400 px"), "native zoom label")
        smaller = wait(lambda: next((node for node in showing("push button") if node.name in ["축소", "−"]), None),
                       "zoom out button")
        bx, by, bw, bh = dimensions(badge)
        sx, sy, _, sh = dimensions(smaller)
        assert bx + bw <= sx and by < sy + sh and sy < by + bh, "zoom label belongs to the left of zoom controls"
        assert not copy_button(), "the copy button is hidden when metadata is closed"
        key("Tab")
        copy = wait(copy_button, "metadata copy button")
        x, y, w, h = dimensions(copy)
        click(win, x + w // 2, y + h // 2)
        clipboard = Gdk.Display.get_default().get_clipboard()
        copied = []
        clipboard.read_text_async(None, lambda cb, result: copied.append(cb.read_text_finish(result)))
        wait(lambda: copied, "read clipboard after clicking copy")
        assert copied == ["Title: Copy <literal>\nSecond line"], copied
        subprocess.run(["xdotool", "mousemove", "--window", win, "400", "320"], check=True)
        key("grave")
        wait(lambda: not info_button() and not page_field(), "hide metadata toggle with top controls")
        assert copy_button(), "an already-open metadata panel stays visible"
        key("grave")
        wait(info_button, "restore metadata toggle with top controls")
        key("plus")
        wait(lambda: zoom_label("125%\n1163 × 500 px"), "zoomed dimensions")
        subprocess.run(["xdotool", "windowsize", win, "800", "360"], check=True)
        wait(lambda: client_size(win) == (800, 360), "manual smaller viewport")
        assert dimensions(showing("image")[0]) == (0, 0, 1163, 500)
        key("Down")
        wait(lambda: "1150×700" in command("xdotool", "getwindowname", win), "second native page")
        assert client_size(win) == (800, 360), "later page must not grow the window"
        assert dimensions(showing("image")[0]) == (0, 0, 1150, 700)
        assert not copy_button(), "copy button must not retain previous page metadata"
        key("9")
        wait(lambda: dimensions(showing("image")[0])[2:] == (800, 360), "explicit fit fills the viewport widget")
        wait(lambda: zoom_label("51.4%\n591 × 360 px"), "fit zoom label")
        key("F11")
        wait(lambda: "_NET_WM_STATE_FULLSCREEN" in command("xprop", "-id", win, "_NET_WM_STATE"),
             "enter fullscreen before saving normal size")
        key("Escape")
        wait(lambda: app.poll() is not None, "exit and save normal size")
        assert app.returncode == 0
        assert json.loads(state_file.read_text()) == {"width": 800, "height": 360}
        app, directory = launch("window-size-restored", inputs["last"], state_home)
        win = window()
        activate(win)
        wait(lambda: "64×96" in command("xdotool", "getwindowname", win), "small startup image")
        assert client_size(win) == (800, 360), "small startup image must not resize the restored window"
        assert dimensions(showing("image")[0]) == (368, 132, 64, 96), "small page is centered"
        key("Escape")
        wait(lambda: app.poll() is not None, "restored viewer exits")
        assert app.returncode == 0
        print("PASS zoom badge, metadata copy, startup growth, clipping and normal-size restoration")

        app, directory = launch("ui", inputs["book"])
        win = window()
        activate(win)
        def page(number):
            wait(lambda: command("xdotool", "getwindowname", win).startswith(f"{number}/3 · "), f"page {number}")
        page(1)
        text = log_snapshot(win, directory)
        assert "backend_ready" in text and "Ready { wayland: false }" in text
        print("PASS context menu log viewer, read-only text, and no log files")
        wait(page_field, "accessible page number field")
        assert not showing("spin button"), "page field must not have duplicate steppers"
        for press, target in [("Down", 2), ("Up", 1), ("space", 2), ("BackSpace", 1), ("Next", 2), ("Prior", 1), ("Tab", 1), ("Down", 2), ("Up", 1)]:
            key(press)
            page(target)
        key("d")
        for press, target in [("Down", 2), ("Up", 1), ("Left", 2), ("Right", 1)]:
            key(press)
            page(target)
        click_page_field(win)
        key("ctrl+a")
        key("2")
        page(1)
        key("Return")
        page(2)
        key("Up")
        page(2)
        key("ctrl+a")
        key("3")
        page(2)
        jump = wait(lambda: next((node for node in showing("push button")
                                 if node.name in ["페이지 이동", "▾"]), None), "page jump button")
        x, y, w, h = dimensions(jump)
        click(win, x + w // 2, y + h // 2)
        page(3)
        click(win, 500, 450)
        key("Up")
        page(2)
        def image_size():
            extent = showing("scroll pane")[0].queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
            return extent.width, extent.height

        normal_size = image_size()
        key("grave")
        wait(lambda: not page_field() and not info_button(), "unpin interface")
        assert image_size() == normal_size, "overlays must not resize the image area"
        key("grave")
        wait(lambda: page_field() and info_button(), "pin interface")

        def fullscreen():
            return "_NET_WM_STATE_FULLSCREEN" in command("xprop", "-id", win, "_NET_WM_STATE")

        key("F11")
        wait(fullscreen, "fullscreen")
        assert page_field() and info_button(), "pinned controls remain visible in fullscreen"
        key("grave")
        wait(lambda: not page_field() and not info_button(), "unpin in fullscreen")
        key("F11")
        wait(lambda: not fullscreen(), "windowed restore")
        assert not page_field() and not info_button(), "unpinned controls stay unpinned after fullscreen"
        key("grave")
        wait(lambda: page_field() and info_button(), "pin again")
        click_page_field(win)
        key("Escape")
        wait(lambda: app.poll() is not None, "exit while editing")
        assert app.returncode == 0
        print("PASS keyboard, explicit focus, top UI visibility and fullscreen restoration")

        trash_space = work / "private-trash"
        trash_space.mkdir()
        delete_files = work / "delete-fixtures"
        delete_files.mkdir()
        originals = [delete_files / f"page{i}.png" for i in range(1, 4)]
        for path in originals:
            path.write_bytes(inputs["last"].read_bytes())
        app, directory = launch("delete", originals[0], data_home=trash_space)
        win = window()
        activate(win)
        wait(lambda: "page1.png · Png" in command("xdotool", "getwindowname", win), "delete source")
        key("Delete")
        wait(delete_dialog, "first deletion confirmation")
        dlg = wait(lambda: command("xdotool", "search", "--onlyvisible", "--name",
                                   "^삭제하시겠습니까").splitlines()[-1], "delete dialog X11 window")
        click_accessible("다시 묻지 않기", dlg, "check box")
        key("Escape")
        wait(lambda: not delete_dialog(), "cancel deletion")
        assert originals[0].is_file()
        activate(win)
        key("Delete")
        wait(delete_dialog, "cancel did not save the checkbox")
        dlg = wait(lambda: command("xdotool", "search", "--onlyvisible", "--name",
                                   "^삭제하시겠습니까").splitlines()[-1], "delete dialog again")
        click_accessible("다시 묻지 않기", dlg, "check box")
        click_accessible("삭제", dlg, "push button")
        wait(lambda: not originals[0].exists(), "first source moved")
        wait(lambda: "page2.png · Png" in command("xdotool", "getwindowname", win), "display next source")
        activate(win)
        subprocess.run(["xdotool", "keydown", "Delete"], check=True)
        try:
            wait(lambda: not originals[1].exists(), "no repeat confirmation this session")
            pump(1.4)
            assert originals[2].is_file(), "holding Del must not delete the following page"
        finally:
            subprocess.run(["xdotool", "keyup", "Delete"], check=True)
        assert not delete_dialog()
        for path in originals[:2]:
            assert (trash_space / "Trash/files" / path.name).read_bytes() == inputs["last"].read_bytes()
        key("Escape")
        wait(lambda: app.poll() is not None, "quit delete session")
        assert originals[2].is_file()
        app, directory = launch("delete-reset", originals[2], data_home=trash_space)
        win = window()
        activate(win)
        wait(lambda: "page3.png · Png" in command("xdotool", "getwindowname", win), "remaining source")
        key("Delete")
        wait(delete_dialog, "confirmation resets in a new process")
        key("Escape")
        activate(win)
        key("Escape")
        wait(lambda: app.poll() is not None, "quit new delete session")
        assert originals[2].is_file()
        book_bytes = inputs["book"].read_bytes()
        app, directory = launch("delete-archive", inputs["book"], data_home=trash_space)
        win = window()
        activate(win)
        page(1)
        key("Delete")
        wait(lambda: any("압축 파일이라 삭제 불가능합니다" in node.name for node in nodes()), "archive warning")
        assert not delete_dialog() and inputs["book"].read_bytes() == book_bytes
        key("Escape")
        activate(win)
        key("Escape")
        wait(lambda: app.poll() is not None, "quit archive session")
        print("PASS delete confirmation, session reset, trash contents and archive warning")

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
