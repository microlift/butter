"""Discord Rich Presence for Roblox, through roserpc.

roserpc (https://github.com/st1rynight/roserpc) is a separate program: it
finds the RobloxPlayer that Rosemary runs, follows its log inside the
prefix Butter set with ``ROSEMARY_PREFIX``, and tells Discord what is being
played. This module only finds it and starts it once; roserpc then idles
while Roblox is closed and leaves Discord alone.
"""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
from pathlib import Path
from typing import Iterable, List, Optional

import butter

NAME = "roserpc"
# Where the README installs it: the AppImage, then the git checkout.
LOCATIONS = ("~/.local/bin/roserpc", "~/roserpc/bin/roserpc")
LOG_NAME = "roserpc.log"
# A roserpc that fails to start (no Python, a bad option) exits at once;
# waiting this long catches that without slowing every launch.
START_GRACE = 1.0
LOG_TAIL_LINES = 5


def find_command(configured: str = "") -> Optional[List[str]]:
    """The command that starts roserpc: the one in the settings, or the
    first roserpc on PATH or in the README's install locations."""
    configured = (configured or "").strip()
    if configured:
        command = shlex.split(configured)
        command[0] = os.path.expanduser(command[0])
        return command
    found = shutil.which(NAME)
    if found:
        return [found]
    for location in LOCATIONS:
        path = Path(location).expanduser()
        if path.is_file() and os.access(path, os.X_OK):
            return [str(path)]
    return None


def is_roserpc(arguments: List[bytes]) -> bool:
    """Whether a command line is roserpc: the AppImage, or the
    ``python -m roserpc`` it and bin/roserpc run. Only the program name is
    matched, so an editor open on ~/roserpc does not count."""
    if arguments and os.path.basename(arguments[0]) == NAME.encode():
        return True
    return any(a == b"-m" and b == NAME.encode() for a, b in zip(arguments, arguments[1:]))


def running(proc: Path = Path("/proc")) -> List[int]:
    """The pids of the roserpc processes this process can see."""
    found = []
    try:
        entries = list(proc.iterdir())
    except OSError:
        return found
    for entry in entries:
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            arguments = [a for a in (entry / "cmdline").read_bytes().split(b"\0") if a]
        except OSError:
            continue
        if is_roserpc(arguments):
            found.append(int(entry.name))
    return found


def _tail(path: Path, lines: int = LOG_TAIL_LINES) -> Iterable[str]:
    try:
        return path.read_text(encoding="utf-8", errors="replace").splitlines()[-lines:]
    except OSError:
        return []


def start(ctx: butter.Context, required: bool = False) -> Optional[str]:
    """Start roserpc unless it is already running. Returns what happened,
    or None if it could not start; with ``required``, that raises instead
    of only being logged."""

    def fail(message: str) -> None:
        if required:
            raise butter.PluginError(message)
        ctx.log(f"Discord Rich Presence: {message}")

    pids = running()
    if pids:
        ctx.log(f"roserpc is already running (pid {pids[0]})")
        return "Discord Rich Presence is already on"

    command = find_command(ctx.setting("roserpc_command", ""))
    if command is None:
        message = (
            "roserpc was not found. Install it from https://github.com/st1rynight/roserpc, "
            "or set the roserpc Command in the Roblox plugin's settings. "
            "Butter's Flatpak comes with it"
        )
        fail(message)
        return None

    ctx.plugin.data_dir.mkdir(parents=True, exist_ok=True)
    log_path = ctx.plugin.data_dir / LOG_NAME
    ctx.log(f"Starting {shlex.join(command)}; its log is {log_path}")
    try:
        with open(log_path, "ab") as log:
            # A session of its own, so it outlives this action and cancelling
            # another action never reaches it. Its output stays out of the
            # action's log, which Butter waits on until it is closed.
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                # Not the plugin's folder, which an update replaces.
                cwd=Path.home(),
                start_new_session=True,
            )
    except OSError as e:
        fail(f"could not start {command[0]}: {e}")
        return None
    try:
        code = process.wait(timeout=START_GRACE)
    except subprocess.TimeoutExpired:
        return "Discord Rich Presence is on"
    for line in _tail(log_path):
        ctx.log(line)
    fail(f"roserpc stopped at once (exit code {code}); see {log_path}")
    return None
