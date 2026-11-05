"""Butter's plugin API.

A plugin is a folder with a ``plugin.toml`` that describes it and a Python
file that implements its actions::

    import butter

    @butter.action("hello")
    def hello(ctx: butter.Context):
        ctx.stage("Saying hello")
        ctx.log("Hello from", ctx.plugin.name)
        return "Said hello"

Butter runs each action in a new Python process. While it runs, the action
reports progress with :meth:`Context.stage`, :meth:`Context.progress` and
:meth:`Context.log`, and asks Butter for things such as bottles, downloads or
settings through the other methods of :class:`Context`. What it asks to
happen in the interface (starting a program, adding a shortcut, showing a
message) is queued and done by Butter once the action returns.

Anything an action prints goes to its log. See ``docs/plugins.md``.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, List, Optional, Sequence

from .archive import extract_zip

__all__ = [
    "API",
    "Bottle",
    "Cancelled",
    "Context",
    "PluginError",
    "PluginInfo",
    "action",
    "extract_zip",
]

#: The version of the protocol between Butter and plugins.
API = 1

Handler = Callable[["Context"], Optional[str]]

#: Actions registered with :func:`action`, by id.
_actions: Dict[str, Handler] = {}


def action(action_id: str) -> Callable[[Handler], Handler]:
    """Register a function as the handler of the action ``action_id``,
    which the plugin's ``plugin.toml`` lists."""

    def register(function: Handler) -> Handler:
        if action_id in _actions:
            raise ValueError(f"action {action_id!r} is registered twice")
        _actions[action_id] = function
        return function

    return register


class PluginError(Exception):
    """An error meant for the user: Butter shows its message as it is,
    without a traceback."""


class Cancelled(BaseException):
    """Raised inside an action when the user cancels it. It derives from
    BaseException, like KeyboardInterrupt, so ``except Exception`` does not
    swallow it; ``finally`` blocks still run."""


class PluginInfo:
    """The running plugin, and the folders it may keep files in."""

    def __init__(self, data: Dict[str, Any]):
        self.id: str = data["id"]
        self.name: str = data["name"]
        self.version: str = data.get("version", "")
        #: The plugin's own folder. It may be read-only.
        self.dir = Path(data["dir"])
        #: Files to keep: they survive updates of the plugin.
        self.data_dir = Path(data["data_dir"])
        #: Files that can be made again, such as downloads.
        self.cache_dir = Path(data["cache_dir"])


class Bottle:
    """A Butter bottle: a Rosemary prefix and its settings."""

    def __init__(self, data: Dict[str, Any]):
        self.id: str = data["id"]
        self.name: str = data["name"]
        #: The bottle's folder, holding ``bottle.toml``, ``prefix/`` and ``logs/``.
        self.dir = Path(data["dir"])
        #: The Rosemary prefix.
        self.prefix = Path(data["prefix"])
        #: The guest's ``/``, as a host path.
        self.drive = Path(data["drive"])
        #: The runner id, or None.
        self.runner: Optional[str] = data.get("runner")
        #: The prefix's guest architecture, ``x86_64`` or ``aarch64``, or
        #: None when it follows the host.
        self.arch: Optional[str] = data.get("arch")
        #: Whether Rosemary has created the prefix.
        self.ready: bool = bool(data.get("ready"))

    @property
    def applications(self) -> Path:
        """The guest's ``/Applications``, as a host path."""
        return self.drive / "Applications"

    def path(self, guest_path: str) -> Path:
        """The host path of ``guest_path``, a path inside the bottle such as
        ``/Applications/Foo.app``."""
        return self.drive / guest_path.lstrip("/")

    def guest_path(self, host_path: os.PathLike) -> str:
        """The path the guest sees ``host_path``, inside the drive, at."""
        relative = Path(host_path).resolve().relative_to(self.drive.resolve())
        return "/" + relative.as_posix() if str(relative) != "." else "/"

    def __repr__(self) -> str:
        return f"Bottle({self.id!r}, name={self.name!r})"


class Context:
    """What an action is given: what it was asked to do, and the way to
    talk to Butter."""

    def __init__(self, request: Dict[str, Any], channel: "Channel"):
        self._channel = channel
        self._effects: List[Dict[str, Any]] = []
        #: The id of the action being run.
        self.action: str = request["action"]
        self.plugin = PluginInfo(request["plugin"])
        #: The plugin's settings: what the user chose, or the defaults.
        self.settings: Dict[str, Any] = dict(request.get("settings") or {})
        #: The bottle the action was run in, for actions shown on a bottle's
        #: page; otherwise None.
        self.bottle: Optional[Bottle] = (
            Bottle(request["bottle"]) if request.get("bottle") else None
        )
        #: Links and files the action was asked to open, as given: from a
        #: shortcut, or ``butter --plugin ID ACTION URL...``.
        self.open: List[str] = list(request.get("open") or [])
        #: Facts about this computer: ``arch`` (``x86_64``, ``aarch64``),
        #: ``flatpak`` (bool).
        self.host: Dict[str, Any] = dict(request.get("host") or {})
        #: Butter's version.
        self.butter_version: str = request.get("butter", "")
        #: Whether Butter's window is open. When it is not, the action was
        #: started from a shortcut or link.
        self.interactive: bool = bool(request.get("interactive"))

    # Progress

    def stage(self, title: str, fraction: Optional[float] = None) -> None:
        """Begin a new step, shown as the progress dialog's heading.
        ``fraction`` is overall progress from 0 to 1, if known."""
        self._channel.send({"t": "stage", "title": str(title), "fraction": fraction})

    def progress(self, fraction: float) -> None:
        """Progress within the current step, from 0 to 1."""
        self._channel.send({"t": "progress", "fraction": float(fraction)})

    def log(self, *parts: Any) -> None:
        """Add a line to the action's log."""
        self._channel.send({"t": "log", "text": " ".join(str(p) for p in parts)})

    # Settings and state

    def setting(self, key: str, default: Any = None) -> Any:
        """A setting's value, or ``default`` if it has none."""
        value = self.settings.get(key)
        return default if value is None else value

    def set_setting(self, key: str, value: Any) -> None:
        """Change one of the plugin's settings, as the user could in its
        settings dialog. ``key`` must be declared in ``plugin.toml``."""
        self._channel.call("settings.set", {"key": key, "value": value})
        self.settings[key] = value

    def load_state(self, name: str = "state") -> Dict[str, Any]:
        """A JSON object the plugin saved with :meth:`save_state`, or ``{}``."""
        try:
            with open(self.plugin.data_dir / f"{name}.json", encoding="utf-8") as f:
                state = json.load(f)
        except (OSError, ValueError):
            return {}
        return state if isinstance(state, dict) else {}

    def save_state(self, state: Dict[str, Any], name: str = "state") -> None:
        """Keep a JSON object in the plugin's data folder."""
        self.plugin.data_dir.mkdir(parents=True, exist_ok=True)
        path = self.plugin.data_dir / f"{name}.json"
        tmp = path.with_suffix(".tmp")
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(state, f, indent=2, sort_keys=True)
        os.replace(tmp, path)

    # Bottles

    def bottles(self) -> List[Bottle]:
        """Every bottle, by name."""
        return [Bottle(b) for b in self._channel.call("bottles.list", {})]

    def find_bottle(self, bottle_id: Optional[str]) -> Optional[Bottle]:
        """The bottle ``bottle_id``, or None if there is none."""
        if not bottle_id:
            return None
        found = self._channel.call("bottles.get", {"id": bottle_id})
        return Bottle(found) if found else None

    def create_bottle(
        self, name: str, runner: Optional[str] = None, arch: Optional[str] = None
    ) -> Bottle:
        """Make a bottle and its prefix, as the New Bottle dialog does. The
        runner defaults to the one chosen in Preferences, or the newest
        installed. ``arch`` is ``x86_64`` or ``aarch64``; None follows the
        host. The prefix may download Rosemary's Darwin base."""
        params: Dict[str, Any] = {"name": name}
        if runner:
            params["runner"] = runner
        if arch:
            params["arch"] = arch
        return Bottle(self._channel.call("bottles.create", params))

    def run(
        self,
        bottle: Bottle,
        target: os.PathLike,
        args: Sequence[str] = (),
        backend: str = "auto",
    ) -> int:
        """Run ``target`` (a host path inside the bottle) with ``rosemary run``
        and wait for it to end. Its output goes to the action's log. Returns
        its exit code. For programs the user uses, see :meth:`launch`."""
        result = self._channel.call(
            "bottles.run",
            {
                "bottle": bottle.id,
                "target": str(target),
                "args": [str(a) for a in args],
                "backend": backend,
            },
        )
        return int(result["code"])

    # Network

    def http_get(self, url: str, headers: Optional[Dict[str, str]] = None) -> str:
        """Fetch ``url`` through Butter and return the body as text. Raises
        :class:`PluginError` unless the server answers with success."""
        result = self._channel.call("http.get", {"url": url, "headers": headers or {}})
        return result["text"]

    def http_json(self, url: str, headers: Optional[Dict[str, str]] = None) -> Any:
        """Fetch ``url`` and parse it as JSON."""
        text = self.http_get(url, headers)
        try:
            return json.loads(text)
        except ValueError as e:
            raise PluginError(f"{url} did not return JSON: {e}") from None

    def download(
        self, url: str, dest: os.PathLike, sha256: Optional[str] = None
    ) -> Path:
        """Download ``url`` to ``dest`` through Butter, showing progress in
        the current step. With ``sha256``, a file that does not match is
        deleted and raises :class:`PluginError`. Returns ``dest``."""
        params: Dict[str, Any] = {"url": url, "dest": str(dest)}
        if sha256:
            params["sha256"] = sha256
        self._channel.call("http.download", params)
        return Path(dest)

    # Applications

    def app_info(self, bundle: os.PathLike) -> Dict[str, Any]:
        """What an ``.app`` bundle's ``Info.plist`` says: ``name``,
        ``version``, ``schemes``, ``mime_types``, ``category``, and ``icon``,
        a PNG Butter made from its ``.icns``, or None."""
        return self._channel.call("apps.info", {"path": str(bundle)})

    # Effects, done by Butter once the action returns

    def launch(
        self,
        bottle: Bottle,
        target: os.PathLike,
        args: Sequence[str] = (),
        open: Iterable[str] = (),
        name: Optional[str] = None,
        backend: str = "auto",
    ) -> None:
        """Start ``target`` in ``bottle`` once the action has succeeded, as
        its Run button does: it shows in the Process Manager. ``open`` lists
        links and files to hand the program, as a shortcut would: links are
        also passed as ``-protocolString``/``-protocolUrl``/``-protocolUri``."""
        self._effects.append(
            {
                "type": "launch",
                "bottle": bottle.id,
                "target": str(target),
                "args": [str(a) for a in args],
                "open": [str(o) for o in open],
                "name": name,
                "backend": backend,
            }
        )

    def add_shortcut(
        self,
        name: str,
        action: str,
        schemes: Iterable[str] = (),
        mime_types: Iterable[str] = (),
        icon: Optional[os.PathLike] = None,
        categories: str = "",
        comment: Optional[str] = None,
    ) -> None:
        """Ask the desktop to add a shortcut that runs this plugin's
        ``action``, once the action has succeeded. The shortcut opens the
        links of ``schemes`` and files of ``mime_types``, which reach the
        action as :attr:`open`. ``icon`` is a PNG or SVG file;
        ``categories`` is a freedesktop list such as ``"Game;"``."""
        self._effects.append(
            {
                "type": "shortcut",
                "name": name,
                "action": action,
                "schemes": list(schemes),
                "mime_types": list(mime_types),
                "icon": str(icon) if icon else None,
                "categories": categories,
                "comment": comment,
            }
        )

    def notify(self, text: str) -> None:
        """Show a short message once the action has finished."""
        self._effects.append({"type": "notify", "text": str(text)})

    def show_location(self, location: "os.PathLike | str") -> None:
        """Open a folder in the file manager, or a web page in the browser,
        once the action has finished."""
        self._effects.append({"type": "open", "location": str(location)})

    # Used by the host

    @property
    def effects(self) -> List[Dict[str, Any]]:
        return list(self._effects)


class Channel:
    """The pipe to Butter: one JSON object per line each way."""

    def __init__(self, reader, writer):
        self._reader = reader
        self._writer = writer
        self._next_id = 0

    def send(self, message: Dict[str, Any]) -> None:
        self._writer.write(json.dumps(message, ensure_ascii=False) + "\n")
        self._writer.flush()

    def receive(self) -> Dict[str, Any]:
        line = self._reader.readline()
        if not line:
            raise Cancelled()
        return json.loads(line)

    def call(self, method: str, params: Dict[str, Any]) -> Any:
        """Ask Butter for something and wait for its answer."""
        self._next_id += 1
        call_id = self._next_id
        self.send({"t": "call", "id": call_id, "method": method, "params": params})
        while True:
            reply = self.receive()
            if reply.get("t") == "reply" and reply.get("id") == call_id:
                break
        if "error" in reply:
            raise PluginError(reply["error"])
        return reply.get("ok")
