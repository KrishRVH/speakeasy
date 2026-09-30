#!/usr/bin/env python3
"""Check the real Linux GUI on a private X server using simulated dictation."""

import argparse
import ctypes
import os
from pathlib import Path
import re
import select
import subprocess
import tempfile
import time


def command(*args, env):
    return subprocess.check_output(args, env=env, text=True, timeout=3)


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def check(executable, startup_only, directory):
    env = os.environ.copy()
    for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DBUS_SESSION_BUS_ADDRESS"):
        env.pop(name, None)
    env["XDG_SESSION_TYPE"] = "x11"
    env["XDG_RUNTIME_DIR"] = str(directory)
    # Keep the 720-DIP Settings window within this private 720-pixel display.
    env["GPUI_X11_SCALE_FACTOR"] = "1"
    # Prefer Mesa's CPU renderer so the check also works without a physical GPU.
    software_drivers = sorted(Path("/usr/share/vulkan/icd.d").glob("lvp_icd*.json"))
    if software_drivers:
        env["VK_ICD_FILENAMES"] = str(software_drivers[0])
    read_fd, write_fd = os.pipe()
    with (directory / "xvfb.log").open("wb") as server_log:
        server = subprocess.Popen(
            ["Xvfb", f":{100 + os.getpid() % 20_000}", "-displayfd", str(write_fd),
             "-screen", "0", "1280x720x24", "-nolisten", "tcp"],
            pass_fds=(write_fd,), stdin=subprocess.DEVNULL, stdout=server_log, stderr=server_log,
        )
        os.close(write_fd)
        try:
            with os.fdopen(read_fd) as display_pipe:
                if not select.select([display_pipe], [], [], 10)[0]:
                    raise RuntimeError("Private X server did not start")
                number = display_pipe.readline().strip()
                if not number.isdecimal():
                    raise RuntimeError("Private X server failed to allocate a display")
                env["DISPLAY"] = f":{number}"
            with (directory / "demo.log").open("wb") as app_log:
                app = subprocess.Popen(
                    [str(executable), "--demo", "--config", str(directory / "settings.json")],
                    env=env, stdin=subprocess.DEVNULL, stdout=app_log, stderr=app_log,
                )
                try:
                    owned = verify(app, env, startup_only)
                    if not startup_only:
                        x11 = ctypes.CDLL("libX11.so.6")
                        x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
                        x11.XOpenDisplay.restype = ctypes.c_void_p
                        x11.XUnmapWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
                        x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
                        x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
                        display = x11.XOpenDisplay(env["DISPLAY"].encode())
                        if not display:
                            raise RuntimeError("Cannot unmap the owned demo windows")
                        try:
                            for handle in owned.values():
                                x11.XUnmapWindow(display, int(handle, 16))
                            x11.XSync(display, 0)
                        finally:
                            x11.XCloseDisplay(display)
                        # Let UnmapNotify remove refresh timers. A hidden GUI
                        # must still exit when its native event socket closes.
                        time.sleep(0.5)
                        stop(server)
                        try:
                            app.wait(timeout=3)
                        except subprocess.TimeoutExpired as error:
                            raise RuntimeError("Hidden demo kept running after display disconnect") from error
                        if app.returncode != 0:
                            raise RuntimeError(f"Hidden display disconnect exited with {app.returncode}")
                        print("PASS: hidden GUI exits cleanly after its private display disconnects.")
                except Exception:
                    # This process only runs the simulated demo; its log contains
                    # no microphone, real transcript, clipboard, or credentials.
                    detail = (directory / "demo.log").read_text(errors="replace").strip()
                    if detail:
                        print("Demo diagnostic:\n" + "\n".join(detail.splitlines()[-12:]))
                    raise
                finally:
                    stop(app)
        finally:
            stop(server)


def verify(app, env, startup_only):
    def alive():
        if app.poll() is not None:
            raise RuntimeError(f"Demo exited with {app.returncode}")

    def belongs_to_demo(pid):
        # AppImage launchers can retain a parent process while the GUI runs in
        # a child. Accept only descendants of the process this check launched.
        while pid > 1:
            if pid == app.pid:
                return True
            try:
                fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
                pid = int(fields[1])
            except (OSError, ValueError, IndexError):
                return False
        return False

    def windows():
        tree = command("xwininfo", "-root", "-tree", env=env)
        found = {}
        for handle, title in re.findall(r'(0x[0-9a-fA-F]+) "(Speakeasy(?: pill)?)"', tree):
            owner = command("xprop", "-id", handle, "_NET_WM_PID", env=env)
            match = re.search(r"= (\d+)$", owner.strip())
            if match and belongs_to_demo(int(match[1])):
                found[title] = handle
        return found

    deadline = time.monotonic() + 10
    owned = {}
    while time.monotonic() < deadline:
        alive()
        owned = windows()
        if set(owned) == {"Speakeasy", "Speakeasy pill"}:
            break
        time.sleep(0.05)
    else:
        raise RuntimeError("Demo did not create both owned GUI windows")
    pill = owned["Speakeasy pill"]
    hints = command("xprop", "-id", pill, "_NET_WM_WINDOW_TYPE", "WM_HINTS", env=env)
    if "_NET_WM_WINDOW_TYPE_NOTIFICATION" not in hints or "accepts input or input focus: False" not in hints:
        raise RuntimeError("Pill lacks its nonactivating notification hints")
    if "Override Redirect State: yes" not in command("xwininfo", "-id", pill, env=env):
        raise RuntimeError("Pill is managed as an ordinary application window")

    x11 = ctypes.CDLL("libX11.so.6")
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XFree.argtypes = [ctypes.c_void_p]
    x11.XGetImage.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_int,
                             ctypes.c_uint, ctypes.c_uint, ctypes.c_ulong, ctypes.c_int]
    x11.XGetImage.restype = ctypes.c_void_p
    x11.XGetPixel.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
    x11.XGetPixel.restype = ctypes.c_ulong
    x11.XDestroyImage.argtypes = [ctypes.c_void_p]
    shape = ctypes.CDLL("libXext.so.6")
    shape.XShapeGetRectangles.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int,
                                        ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_int)]
    shape.XShapeGetRectangles.restype = ctypes.c_void_p
    display = x11.XOpenDisplay(env["DISPLAY"].encode())
    if not display:
        raise RuntimeError("Cannot inspect the private display")
    try:
        count, ordering = ctypes.c_int(), ctypes.c_int()
        rectangles = shape.XShapeGetRectangles(display, int(pill, 16), 2,
                                              ctypes.byref(count), ctypes.byref(ordering))
        if rectangles:
            x11.XFree(rectangles)
        if count.value != 0:
            raise RuntimeError("Pill input shape intercepts clicks")
        # Actual pixels catch a live process whose renderer never presents.
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            alive()
            # Window metadata can arrive before mapping; XGetImage requires a
            # viewable window, otherwise Xlib terminates this checker on BadMatch.
            if "Map State: IsViewable" not in command("xwininfo", "-id", owned["Speakeasy"], env=env):
                time.sleep(0.05)
                continue
            image = x11.XGetImage(display, int(owned["Speakeasy"], 16), 0, 0, 400, 300,
                                 ctypes.c_ulong(-1).value, 2)
            if image:
                try:
                    colors = {x11.XGetPixel(image, x, y) for x in range(20, 400, 25)
                              for y in range(20, 300, 25)}
                    if len(colors) > 1:
                        break
                finally:
                    x11.XDestroyImage(image)
            time.sleep(0.05)
        else:
            raise RuntimeError("Settings never presented rendered content")
    finally:
        x11.XCloseDisplay(display)
    if startup_only:
        alive()
        print("PASS: real GUI startup, Settings rendering, and nonactivating click-through pill.")
        return owned
    # The automatic preview exercises repeated show/hide and ends with an
    # eight-second error hint. It must then leave the pill natively unmapped.
    seen_visible = False
    deadline = time.monotonic() + 35
    while time.monotonic() < deadline:
        alive()
        mapped = "Map State: IsViewable" in command("xwininfo", "-id", pill, env=env)
        seen_visible |= mapped
        if seen_visible and not mapped and time.monotonic() > deadline - 12:
            print("PASS: GUI startup, rendering, click-through pill, and preview show/hide completion.")
            return owned
        time.sleep(0.1)
    raise RuntimeError("Preview did not show and finally unmap the pill")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("--startup-only", action="store_true", help="skip the complete simulated preview")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="speakeasy-demo-") as temporary:
        check(args.executable.resolve(strict=True), args.startup_only, Path(temporary))


if __name__ == "__main__":
    main()
