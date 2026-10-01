#!/usr/bin/env python3
"""Check the real Linux GUI on a private X server using simulated dictation."""

import argparse
import ctypes
import os
import re
import select
import signal
import subprocess
import tempfile
import time
from collections.abc import Callable, Iterable, Mapping
from io import TextIOWrapper
from pathlib import Path
from types import TracebackType
from typing import Protocol, Self, cast


class NativeFunction[**P, T](Protocol):
    """A ctypes function whose Python arguments and result match its bound C ABI."""

    argtypes: list[type[object]]
    restype: type[object]

    def __call__(self, *args: P.args, **kwargs: P.kwargs) -> T: ...


class X11(Protocol):
    XOpenDisplay: NativeFunction[[bytes], int | None]
    XCloseDisplay: NativeFunction[[int], int]
    XUnmapWindow: NativeFunction[[int, int], int]
    XSync: NativeFunction[[int, int], int]
    XFree: NativeFunction[[int], int]
    XGetImage: NativeFunction[[int, int, int, int, int, int, int, int], int | None]
    XGetPixel: NativeFunction[[int, int, int], int]
    XDestroyImage: NativeFunction[[int], int]


class Xext(Protocol):
    XShapeGetRectangles: NativeFunction[[int, int, int, object, object], int | None]


class Display:
    """Own one private display and release every Xlib allocation before closing it."""

    def __init__(self, name: str) -> None:
        # These casts declare the ABI bound below; ctypes cannot derive C signatures.
        self._x11: X11 = cast(X11, cast(object, ctypes.CDLL("libX11.so.6")))
        self._shape: Xext = cast(Xext, cast(object, ctypes.CDLL("libXext.so.6")))
        self._x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
        self._x11.XOpenDisplay.restype = ctypes.c_void_p
        self._x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
        self._x11.XUnmapWindow.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        self._x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self._x11.XFree.argtypes = [ctypes.c_void_p]
        self._x11.XGetImage.argtypes = [
            ctypes.c_void_p,
            ctypes.c_ulong,
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_uint,
            ctypes.c_uint,
            ctypes.c_ulong,
            ctypes.c_int,
        ]
        self._x11.XGetImage.restype = ctypes.c_void_p
        self._x11.XGetPixel.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int]
        self._x11.XGetPixel.restype = ctypes.c_ulong
        self._x11.XDestroyImage.argtypes = [ctypes.c_void_p]
        self._shape.XShapeGetRectangles.argtypes = [
            ctypes.c_void_p,
            ctypes.c_ulong,
            ctypes.c_int,
            ctypes.POINTER(ctypes.c_int),
            ctypes.POINTER(ctypes.c_int),
        ]
        self._shape.XShapeGetRectangles.restype = ctypes.c_void_p
        display = self._x11.XOpenDisplay(name.encode())
        if display is None:
            raise RuntimeError("Cannot open the private display")
        self._display: int = display

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        _: type[BaseException] | None,
        __: BaseException | None,
        ___: TracebackType | None,
    ) -> None:
        self._x11.XCloseDisplay(self._display)

    def unmap(self, handles: Iterable[str]) -> None:
        for handle in handles:
            self._x11.XUnmapWindow(self._display, int(handle, 16))
        self._x11.XSync(self._display, 0)

    def check_input_shape(self, handle: str) -> None:
        count, ordering = ctypes.c_int(), ctypes.c_int()
        rectangles = self._shape.XShapeGetRectangles(
            self._display,
            int(handle, 16),
            2,
            ctypes.byref(count),
            ctypes.byref(ordering),
        )
        if rectangles is not None:
            self._x11.XFree(rectangles)
        if count.value != 0:
            raise RuntimeError("Pill input shape intercepts clicks")

    def rendered(self, handle: str) -> bool:
        image = self._x11.XGetImage(
            self._display,
            int(handle, 16),
            0,
            0,
            400,
            300,
            ctypes.c_ulong(-1).value,
            2,
        )
        if image is None:
            return False
        try:
            colors = {
                self._x11.XGetPixel(image, x, y)
                for x in range(20, 400, 25)
                for y in range(20, 300, 25)
            }
            return len(colors) > 1
        finally:
            self._x11.XDestroyImage(image)


def command(*args: str, env: Mapping[str, str]) -> str:
    return subprocess.check_output(args, env=env, text=True, timeout=3)


def stop(process: subprocess.Popen[bytes]) -> None:
    """Reap the owned session leader and terminate its launcher descendants."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
    finally:
        # The leader can exit while a launcher child remains in the owned group.
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def check(
    executable: Path,
    startup_only: bool,
    directory: Path,
    environment: Mapping[str, str],
    now: Callable[[], float],
    sleep: Callable[[float], None],
) -> None:
    env = dict(environment)
    for name in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DBUS_SESSION_BUS_ADDRESS", "ZED_HEADLESS"):
        env.pop(name, None)
    env["XDG_SESSION_TYPE"] = "x11"
    env["XDG_RUNTIME_DIR"] = str(directory)
    # Keep the 720-DIP Settings window within this private 720-pixel display.
    env["GPUI_X11_SCALE_FACTOR"] = "1"
    # Prefer Mesa's CPU renderer so the check also works without a physical GPU.
    software_drivers = sorted(Path("/usr/share/vulkan/icd.d").glob("lvp_icd*.json"))
    if software_drivers:
        env["VK_ICD_FILENAMES"] = str(software_drivers[0])
    with (directory / "xvfb.log").open("wb") as server_log:
        read_fd, write_fd = os.pipe()
        with os.fdopen(read_fd) as display_pipe, os.fdopen(write_fd, "wb") as display_output:
            server = subprocess.Popen(
                [
                    "Xvfb",
                    f":{100 + os.getpid() % 20_000}",
                    "-displayfd",
                    str(write_fd),
                    "-screen",
                    "0",
                    "1280x720x24",
                    "-nolisten",
                    "tcp",
                ],
                pass_fds=(write_fd,),
                stdin=subprocess.DEVNULL,
                stdout=server_log,
                stderr=server_log,
                start_new_session=True,
            )
            display_output.close()
            try:
                check_server(
                    server, display_pipe, executable, startup_only, directory, env, now, sleep
                )
            finally:
                stop(server)


def check_server(
    server: subprocess.Popen[bytes],
    display_pipe: TextIOWrapper,
    executable: Path,
    startup_only: bool,
    directory: Path,
    env: dict[str, str],
    now: Callable[[], float],
    sleep: Callable[[float], None],
) -> None:
    if not select.select([display_pipe], [], [], 10)[0]:
        raise RuntimeError("Private X server did not start")
    number = display_pipe.readline().strip()
    if not number.isdecimal():
        raise RuntimeError("Private X server failed to allocate a display")
    env["DISPLAY"] = f":{number}"
    with (directory / "demo.log").open("wb") as app_log:
        app = subprocess.Popen(
            [str(executable), "--demo", "--config", str(directory / "settings.json")],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=app_log,
            stderr=app_log,
            start_new_session=True,
        )
        try:
            owned = verify(app, env, startup_only, now, sleep)
            if not startup_only:
                with Display(env["DISPLAY"]) as display:
                    display.unmap(owned.values())
                # Let UnmapNotify remove refresh timers before closing the socket.
                sleep(0.5)
                stop(server)
                try:
                    app.wait(timeout=3)
                except subprocess.TimeoutExpired as error:
                    raise RuntimeError(
                        "Hidden demo kept running after display disconnect"
                    ) from error
                if app.returncode != 0:
                    raise RuntimeError(f"Hidden display disconnect exited with {app.returncode}")
                print("PASS: hidden GUI exits cleanly after its private display disconnects.")
        except Exception:
            # Only simulated demo output: no live audio, transcript, or credentials.
            detail = (directory / "demo.log").read_text(errors="replace").strip()
            if detail:
                print("Demo diagnostic:\n" + "\n".join(detail.splitlines()[-12:]))
            raise
        finally:
            stop(app)


def belongs_to_demo(pid: int, owner: int, proc: Path) -> bool:
    """Accept only this launcher's descendants, rejecting vanished or cyclic ancestry."""
    seen: set[int] = set()
    while pid > 1 and pid not in seen:
        if pid == owner:
            return True
        seen.add(pid)
        try:
            fields = (proc / str(pid) / "stat").read_text().rsplit(")", 1)[1].split()
            pid = int(fields[1])
        except (OSError, ValueError, IndexError):
            return False
    return False


def verify(
    app: subprocess.Popen[bytes],
    env: Mapping[str, str],
    startup_only: bool,
    now: Callable[[], float],
    sleep: Callable[[float], None],
) -> dict[str, str]:
    def alive() -> None:
        if app.poll() is not None:
            raise RuntimeError(f"Demo exited with {app.returncode}")

    def windows() -> dict[str, str]:
        tree = command("xwininfo", "-root", "-tree", env=env)
        found: dict[str, str] = {}
        for window in re.finditer(r'(0x[0-9a-fA-F]+) "(Speakeasy(?: pill)?)"', tree):
            handle, title = window.group(1, 2)
            owner = command("xprop", "-id", handle, "_NET_WM_PID", env=env)
            match = re.search(r"= (\d+)$", owner.strip())
            if match and belongs_to_demo(int(match[1]), app.pid, Path("/proc")):
                found[title] = handle
        return found

    deadline = now() + 10
    owned: dict[str, str] = {}
    while now() < deadline:
        alive()
        owned = windows()
        if set(owned) == {"Speakeasy", "Speakeasy pill"}:
            break
        sleep(0.05)
    else:
        raise RuntimeError("Demo did not create both owned GUI windows")
    pill = owned["Speakeasy pill"]
    hints = command("xprop", "-id", pill, "_NET_WM_WINDOW_TYPE", "WM_HINTS", env=env)
    if (
        "_NET_WM_WINDOW_TYPE_NOTIFICATION" not in hints
        or "accepts input or input focus: False" not in hints
    ):
        raise RuntimeError("Pill lacks its nonactivating notification hints")
    if "Override Redirect State: yes" not in command("xwininfo", "-id", pill, env=env):
        raise RuntimeError("Pill is managed as an ordinary application window")

    with Display(env["DISPLAY"]) as display:
        display.check_input_shape(pill)
        # Actual pixels catch a live process whose renderer never presents.
        deadline = now() + 5
        while now() < deadline:
            alive()
            # Window metadata can arrive before mapping; XGetImage requires a
            # viewable window, otherwise Xlib terminates this checker on BadMatch.
            if "Map State: IsViewable" not in command(
                "xwininfo", "-id", owned["Speakeasy"], env=env
            ):
                sleep(0.05)
                continue
            if display.rendered(owned["Speakeasy"]):
                break
            sleep(0.05)
        else:
            raise RuntimeError("Settings never presented rendered content")
    if startup_only:
        alive()
        print("PASS: real GUI startup, Settings rendering, and nonactivating click-through pill.")
        return owned
    # The automatic preview exercises repeated show/hide and ends with an
    # eight-second error hint. It must then leave the pill natively unmapped.
    seen_visible = False
    deadline = now() + 35
    while now() < deadline:
        alive()
        mapped = "Map State: IsViewable" in command("xwininfo", "-id", pill, env=env)
        seen_visible |= mapped
        if seen_visible and not mapped and now() > deadline - 12:
            print(
                "PASS: GUI startup, rendering, click-through pill, and preview show/hide completion."
            )
            return owned
        sleep(0.1)
    raise RuntimeError("Preview did not show and finally unmap the pill")


class Arguments(argparse.Namespace):
    def __init__(self) -> None:
        super().__init__()
        self.executable: Path = Path(".")
        self.startup_only: bool = False


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument(
        "--startup-only", action="store_true", help="skip the complete simulated preview"
    )
    args = parser.parse_args(namespace=Arguments())
    # The composition root snapshots ambient config for owned child processes.
    environment = dict(os.environ)  # noqa: TID251
    with tempfile.TemporaryDirectory(prefix="speakeasy-demo-") as temporary:
        check(
            args.executable.resolve(strict=True),
            args.startup_only,
            Path(temporary),
            environment,
            time.monotonic,
            time.sleep,
        )


if __name__ == "__main__":
    main()
