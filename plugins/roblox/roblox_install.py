"""Installing Roblox into a bottle, and preparing it to run.

An app is unpacked next to where it goes, in a hidden folder of the bottle's
``/Applications``, and moved into place only once it is complete, so a
cancelled or failed update leaves the old version working.

The plugin's records (``state.json`` in its data folder) say, for each
bottle, which version, channel and build of each app is installed, and which
of its files mods replaced. The files mods replaced are kept in
``pristine/<bottle>/<app>/``, so they come back when a mod is removed.
"""

from __future__ import annotations

import json
import os
import shutil
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

import butter
from roblox_deploy import (
    LIVE,
    Component,
    DeployError,
    choose_arch,
    link_channel,
    normalize_channel,
    normalize_version,
    package_url,
    version_url,
)

#: Inside an app bundle, where FastFlags are read from.
FFLAGS_FILE = Path("Contents/MacOS/ClientSettings/ClientAppSettings.json")
#: Inside an app bundle, the folder mods replace files in.
RESOURCES = Path("Contents/Resources")

RENDERER_FLAGS = {
    "metal": {"FFlagDebugGraphicsPreferMetal": "True"},
    "opengl": {"FFlagDebugGraphicsPreferOpenGL": "True"},
}


# --- Records ---


def record(ctx: butter.Context, bottle: butter.Bottle, component: Component) -> Dict[str, Any]:
    """What is recorded of ``component`` in ``bottle``, or ``{}``."""
    return ctx.load_state().get("installs", {}).get(bottle.id, {}).get(component.key, {})


def save_record(
    ctx: butter.Context, bottle: butter.Bottle, component: Component, entry: Optional[Dict[str, Any]]
) -> None:
    state = ctx.load_state()
    installs = state.setdefault("installs", {}).setdefault(bottle.id, {})
    if entry is None:
        installs.pop(component.key, None)
    else:
        installs[component.key] = entry
    ctx.save_state(state)


def app_path(bottle: butter.Bottle, component: Component) -> Path:
    return bottle.applications / component.app_name


def is_installed(bottle: butter.Bottle, component: Component) -> bool:
    return (app_path(bottle, component) / "Contents").is_dir()


# --- Choosing what to install ---


def channel_of(ctx: butter.Context, links=()) -> str:
    """The channel to install from: the one in a link, if the user lets
    links choose, or the one in the settings."""
    if ctx.setting("follow_link_channel", False):
        from_link = link_channel(links)
        if from_link:
            return from_link
    try:
        return normalize_channel(ctx.setting("channel", LIVE))
    except DeployError as e:
        raise butter.PluginError(f"Check the Channel setting: {e}") from None


def arch_of(ctx: butter.Context, bottle: butter.Bottle) -> str:
    return choose_arch(ctx.setting("architecture"), bottle.arch, ctx.host.get("arch", ""))


def pinned_version(ctx: butter.Context) -> Optional[str]:
    """The version the settings pin, if any."""
    pinned = (ctx.setting("version") or "").strip()
    if not pinned:
        return None
    try:
        return normalize_version(pinned)
    except DeployError as e:
        raise butter.PluginError(f"Check the Version setting: {e}") from None


def newest_version(ctx: butter.Context, component: Component, channel: str) -> tuple:
    """The newest version of ``channel`` and the channel it comes from. A
    channel Roblox won't say anything about (they can be private) falls
    back to LIVE."""
    try:
        return _newest(ctx, component, channel), channel
    except butter.PluginError as e:
        if channel == LIVE:
            raise
        ctx.log(f"The {channel} channel is not available ({e}); using LIVE")
        return _newest(ctx, component, LIVE), LIVE


def _newest(ctx: butter.Context, component: Component, channel: str) -> str:
    url = version_url(component, channel)
    data = ctx.http_json(url)
    version = data.get("clientVersionUpload") if isinstance(data, dict) else None
    if not version:
        raise butter.PluginError(f"{url} named no version")
    try:
        return normalize_version(version)
    except DeployError as e:
        raise butter.PluginError(str(e)) from None


# --- Installing ---


def ensure(
    ctx: butter.Context,
    bottle: butter.Bottle,
    component: Component,
    links=(),
    update: bool = True,
    force: bool = False,
) -> Path:
    """Make sure ``component`` is installed in ``bottle``, updating it if
    ``update`` and reinstalling it if ``force``. Returns the app's path."""
    app = app_path(bottle, component)
    installed = is_installed(bottle, component)
    if installed and not update and not force:
        return app
    channel = channel_of(ctx, links)
    arch = arch_of(ctx, bottle)
    pinned = pinned_version(ctx)
    if pinned:
        version = pinned
    else:
        ctx.stage(f"Checking for {component.title} updates")
        try:
            version, channel = newest_version(ctx, component, channel)
        except butter.PluginError as e:
            if installed and not force:
                ctx.log(f"Could not check for updates ({e}); starting the installed version")
                return app
            raise
    current = record(ctx, bottle, component)
    if (
        installed
        and not force
        and current.get("version") == version
        and current.get("arch") == arch
    ):
        ctx.log(f"{component.title} {version} is up to date")
        return app
    install(ctx, bottle, component, version, channel, arch)
    return app


def install(
    ctx: butter.Context,
    bottle: butter.Bottle,
    component: Component,
    version: str,
    channel: str,
    arch: str,
) -> Path:
    """Download and install ``component`` at ``version`` into ``bottle``."""
    url = package_url(component, version, channel, arch)
    downloads = ctx.plugin.cache_dir / "downloads"
    archive = downloads / f"{channel}-{arch}-{version}-{component.package}"
    ctx.stage(f"Downloading {component.title} {version}", 0.1)
    ctx.download(url, archive)

    ctx.stage(f"Installing {component.title}", 0.6)
    apps = bottle.applications
    apps.mkdir(parents=True, exist_ok=True)
    staging = apps / f".butter-roblox-{component.key}.new"
    old = apps / f".butter-roblox-{component.key}.old"
    for leftover in (staging, old):
        if leftover.exists():
            shutil.rmtree(leftover)
    try:
        butter.extract_zip(archive, staging, progress=ctx.progress)
        unpacked = find_app(staging)
        if unpacked is None:
            raise butter.PluginError(f"{url} holds no app")
        dest = app_path(bottle, component)
        if dest.exists() or dest.is_symlink():
            os.rename(dest, old)
        os.rename(unpacked, dest)
    finally:
        for leftover in (staging, old):
            if leftover.exists():
                shutil.rmtree(leftover, ignore_errors=True)
    archive.unlink(missing_ok=True)
    # A new copy has none of the mods; they are applied again before it runs.
    shutil.rmtree(pristine_dir(ctx, bottle, component), ignore_errors=True)
    save_record(
        ctx,
        bottle,
        component,
        {
            "version": version,
            "channel": channel,
            "arch": arch,
            "installed": int(time.time()),
            "mods": [],
        },
    )
    ctx.log(f"Installed {component.title} {version} ({channel}, {arch})")
    return dest


def find_app(folder: Path) -> Optional[Path]:
    """The app bundle in an unpacked zip: at its top, or one folder down."""
    for depth in (folder, *sorted(p for p in folder.iterdir() if p.is_dir())):
        apps = sorted(p for p in depth.iterdir() if p.suffix == ".app" and p.is_dir())
        if apps:
            return apps[0]
    return None


def uninstall(ctx: butter.Context, bottle: butter.Bottle, component: Component) -> None:
    app = app_path(bottle, component)
    if app.exists():
        shutil.rmtree(app)
    shutil.rmtree(pristine_dir(ctx, bottle, component), ignore_errors=True)
    save_record(ctx, bottle, component, None)


# --- Preparing to run ---


def fflags(ctx: butter.Context, component: Component) -> Dict[str, Any]:
    """The FastFlags to write: the renderer's, then the user's own."""
    flags: Dict[str, Any] = dict(RENDERER_FLAGS.get(ctx.setting("renderer", "auto"), {}))
    text = (ctx.setting(component.fflags_setting) or "").strip()
    if text:
        try:
            own = json.loads(text)
        except ValueError as e:
            raise butter.PluginError(
                f"The {component.title} FastFlags are not valid JSON: {e}"
            ) from None
        if not isinstance(own, dict):
            raise butter.PluginError(f"The {component.title} FastFlags must be a JSON object")
        flags.update(own)
    return flags


def write_fflags(app: Path, flags: Dict[str, Any]) -> Path:
    """Write ``flags`` as the app's ``ClientAppSettings.json``; with no
    flags, remove it."""
    path = app / FFLAGS_FILE
    if not flags:
        path.unlink(missing_ok=True)
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(flags, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(tmp, path)
    return path


def mods_dir(ctx: butter.Context, component: Component) -> Path:
    return ctx.plugin.data_dir / "mods" / component.key


def pristine_dir(ctx: butter.Context, bottle: butter.Bottle, component: Component) -> Path:
    return ctx.plugin.data_dir / "pristine" / bottle.id / component.key


def mod_files(folder: Path) -> List[str]:
    """The files under ``folder``, as relative POSIX paths."""
    if not folder.is_dir():
        return []
    return sorted(
        p.relative_to(folder).as_posix()
        for p in folder.rglob("*")
        if p.is_file() and not p.name.startswith(".")
    )


def apply_mods(
    ctx: butter.Context, bottle: butter.Bottle, component: Component, enabled: bool = True
) -> int:
    """Copy the mods into the app's ``Contents/Resources``, keeping each
    file they replace, and put back the files of mods since removed.
    Returns how many files are modded."""
    app = app_path(bottle, component)
    resources = (app / RESOURCES).resolve()
    source = mods_dir(ctx, component)
    pristine = pristine_dir(ctx, bottle, component)
    entry = record(ctx, bottle, component)
    before = set(entry.get("mods", []))
    wanted = set(mod_files(source)) if enabled else set()

    for rel in sorted(before - wanted):
        target = resources / rel
        kept = pristine / rel
        if kept.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(kept, target)
            kept.unlink()
        else:
            # The mod added the file; the app had none.
            target.unlink(missing_ok=True)
    for rel in sorted(wanted):
        target = (resources / rel).resolve()
        if os.path.commonpath([resources, target]) != str(resources):
            ctx.log(f"Skipped the mod {rel}: it is outside the app")
            wanted.discard(rel)
            continue
        kept = pristine / rel
        if rel not in before and target.is_file() and not kept.exists():
            kept.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(target, kept)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / rel, target)
    if entry or wanted:
        entry = dict(entry)
        entry["mods"] = sorted(wanted)
        save_record(ctx, bottle, component, entry)
    if wanted:
        ctx.log(f"Applied {len(wanted)} modded file(s) to {component.title}")
    return len(wanted)


def prepare(ctx: butter.Context, bottle: butter.Bottle, component: Component) -> Path:
    """Apply FastFlags and mods before ``component`` runs."""
    app = app_path(bottle, component)
    ctx.stage(f"Preparing {component.title}", 0.9)
    flags = fflags(ctx, component)
    write_fflags(app, flags)
    if flags:
        ctx.log(f"Set {len(flags)} FastFlag(s)")
    apply_mods(ctx, bottle, component, enabled=bool(ctx.setting("mods", True)))
    return app
