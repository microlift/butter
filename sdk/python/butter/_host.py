"""Runs one action of a plugin for Butter.

Butter starts ``python3 -I -B -u -X utf8 -c BOOT SDK_DIR``, where BOOT puts
SDK_DIR on ``sys.path`` and calls :func:`main`. Butter then writes one
``invoke`` request to standard input. From here on, standard output carries
only messages to Butter; anything the plugin prints, and the output of
programs it starts, goes to standard error, which Butter adds to the log.
"""

from __future__ import annotations

import importlib.util
import os
import signal
import sys
import traceback

import butter

MIN_PYTHON = (3, 9)


def _private(fd: int, mode: str):
    """A file object for a copy of ``fd``, which the plugin and the programs
    it starts can't reach through the standard descriptors."""
    copy = os.dup(fd)
    os.set_inheritable(copy, False)
    return os.fdopen(copy, mode, encoding="utf-8", buffering=1)


def main() -> None:
    to_butter = _private(1, "w")
    from_butter = _private(0, "r")
    # print() and child processes write to the log; they read nothing.
    os.dup2(2, 1)
    null = os.open(os.devnull, os.O_RDONLY)
    os.dup2(null, 0)
    os.close(null)

    channel = butter.Channel(from_butter, to_butter)

    def cancel(*_):
        raise butter.Cancelled()

    signal.signal(signal.SIGTERM, cancel)
    signal.signal(signal.SIGINT, cancel)

    try:
        request = channel.receive()
    except butter.Cancelled:
        return
    if request.get("t") != "invoke":
        channel.send({"t": "fail", "message": "expected an invoke request"})
        return
    if sys.version_info < MIN_PYTHON:
        found = ".".join(map(str, sys.version_info[:3]))
        wanted = ".".join(map(str, MIN_PYTHON))
        channel.send(
            {"t": "fail", "message": f"plugins need Python {wanted} or newer; found {found}"}
        )
        return
    wanted_api = int(request.get("api", butter.API))
    if wanted_api > butter.API:
        channel.send(
            {"t": "fail", "message": f"this plugin needs a newer Butter (plugin API {wanted_api})"}
        )
        return

    try:
        result = _run(request, channel)
    except butter.Cancelled:
        channel.send({"t": "fail", "message": "cancelled"})
        return
    except butter.PluginError as e:
        channel.send({"t": "fail", "message": str(e) or type(e).__name__})
        return
    except Exception as e:  # noqa: BLE001 - everything else is a bug to report
        traceback.print_exc()
        sys.stderr.flush()
        channel.send({"t": "fail", "message": f"{type(e).__name__}: {e}"})
        return
    channel.send(result)


def _run(request, channel) -> dict:
    plugin = request["plugin"]
    directory = plugin["dir"]
    entry = os.path.join(directory, request.get("entry") or "plugin.py")
    # The plugin may keep helpers next to its entry file.
    sys.path.insert(0, directory)
    spec = importlib.util.spec_from_file_location("butter_plugin", entry)
    if spec is None or spec.loader is None:
        raise butter.PluginError(f"could not load {entry}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["butter_plugin"] = module
    spec.loader.exec_module(module)

    action = request["action"]
    handler = butter._actions.get(action)
    if handler is None:
        raise butter.PluginError(f"{plugin['name']} has no handler for the action {action!r}")
    ctx = butter.Context(request, channel)
    message = handler(ctx)
    return {
        "t": "done",
        "message": message if isinstance(message, str) else None,
        "effects": ctx.effects,
    }
