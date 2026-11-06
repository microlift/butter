"""A Roblox and Roblox Studio bootstrapper for Butter.

* **Set Up Roblox** makes a bottle (unless one is chosen in the settings),
  installs Roblox, and Roblox Studio if the settings ask for it, and adds
  them to the desktop's applications menu. The menu entries run this
  plugin, so the desktop sends it the ``roblox-player:``, ``roblox:``,
  ``roblox-studio:`` and ``roblox-studio-auth:`` links the website opens.
* **Launch Roblox** and **Launch Roblox Studio** update the app if a newer
  version is out, write its FastFlags, apply mods, and start it with the
  link that launched it, if any. With Discord Rich Presence on (the
  default), Launch Roblox also starts roserpc, which shows the game on Discord.
"""

from __future__ import annotations

from typing import List, Optional

import butter
import roblox_install as rbx
import roblox_rpc
from roblox_deploy import COMPONENTS, PLAYER, STUDIO, Component, component_for_link

BOTTLE_NAME = "Roblox"


# --- The bottle ---


def bottle_for(ctx: butter.Context, create: bool = True) -> butter.Bottle:
    """The bottle Roblox lives in: the one in the settings, or else one
    named Roblox, which is made if there is none."""
    chosen = ctx.setting("bottle")
    bottle = ctx.find_bottle(chosen)
    if bottle is None and chosen:
        ctx.log(f"The bottle {chosen!r} in the settings no longer exists")
    if bottle is None:
        bottle = next((b for b in ctx.bottles() if b.name.lower() == BOTTLE_NAME.lower()), None)
        if bottle is not None:
            ctx.log(f"Using the existing {bottle.name} bottle")
    if bottle is None:
        if not create:
            raise butter.PluginError("Roblox is not set up yet. Run Set Up Roblox first")
        ctx.stage(f"Making the {BOTTLE_NAME} bottle")
        bottle = ctx.create_bottle(BOTTLE_NAME)
    if bottle.id != chosen:
        ctx.set_setting("bottle", bottle.id)
    if not bottle.ready:
        raise butter.PluginError(
            f"The {bottle.name} bottle has no prefix yet. Open it in Butter and choose Create"
        )
    return bottle


# --- Shortcuts ---


def add_shortcut(ctx: butter.Context, bottle: butter.Bottle, component: Component) -> None:
    """Add ``component`` to the applications menu, opening its links."""
    icon = None
    try:
        icon = ctx.app_info(rbx.app_path(bottle, component)).get("icon")
    except butter.PluginError as e:
        ctx.log(f"No icon for the {component.title} shortcut: {e}")
    ctx.add_shortcut(
        name=component.title,
        action=component.key,
        schemes=component.schemes,
        icon=icon or (ctx.plugin.dir / "icon.svg"),
        categories=component.categories,
        comment=f"Start {component.title} in the {bottle.name} bottle",
    )


# --- Actions ---


def links_for(ctx: butter.Context, component: Component) -> List[str]:
    """The links the action was given that are meant for ``component``.
    Files and other links are passed on as they are."""
    return [o for o in ctx.open if component_for_link(o) in (None, component)]


def launch(ctx: butter.Context, component: Component) -> str:
    links = links_for(ctx, component)
    bottle = bottle_for(ctx)
    rbx.ensure(
        ctx,
        bottle,
        component,
        links=links,
        update=bool(ctx.setting("update_on_launch", True)),
    )
    app = rbx.prepare(ctx, bottle, component)
    if component is PLAYER and ctx.setting("discord_rpc", True):
        roblox_rpc.start(ctx)
    ctx.launch(bottle, app, open=links, name=component.title)
    return f"Starting {component.title}"


@butter.action("player")
def player(ctx: butter.Context) -> str:
    return launch(ctx, PLAYER)


@butter.action("studio")
def studio(ctx: butter.Context) -> str:
    return launch(ctx, STUDIO)


def install_all(ctx: butter.Context, bottle: butter.Bottle, force: bool = False) -> List[Component]:
    wanted = [PLAYER]
    if ctx.setting("studio", False) or rbx.is_installed(bottle, STUDIO):
        wanted.append(STUDIO)
    for component in wanted:
        rbx.ensure(ctx, bottle, component, update=True, force=force)
        rbx.prepare(ctx, bottle, component)
    return wanted


@butter.action("setup")
def setup(ctx: butter.Context, bottle: Optional[butter.Bottle] = None) -> str:
    bottle = bottle or bottle_for(ctx)
    installed = install_all(ctx, bottle)
    for component in installed:
        add_shortcut(ctx, bottle, component)
    names = " and ".join(c.title for c in installed)
    ctx.notify(f"{names} installed in {bottle.name}")
    return f"{names} installed in the {bottle.name} bottle"


@butter.action("install-here")
def install_here(ctx: butter.Context) -> str:
    if ctx.bottle is None:
        raise butter.PluginError("Run this from a bottle's page")
    ctx.set_setting("bottle", ctx.bottle.id)
    if not ctx.bottle.ready:
        raise butter.PluginError(f"{ctx.bottle.name} has no prefix yet; create it first")
    return setup(ctx, ctx.bottle)


@butter.action("update")
def update(ctx: butter.Context) -> str:
    bottle = bottle_for(ctx, create=False)
    found = [c for c in COMPONENTS if rbx.is_installed(bottle, c)]
    if not found:
        raise butter.PluginError(f"Roblox is not installed in {bottle.name}; run Set Up Roblox")
    lines = []
    for component in found:
        before = rbx.record(ctx, bottle, component).get("version")
        rbx.ensure(ctx, bottle, component, update=True)
        rbx.prepare(ctx, bottle, component)
        after = rbx.record(ctx, bottle, component).get("version")
        if before == after:
            lines.append(f"{component.title} is up to date")
        else:
            lines.append(f"{component.title} was updated to {after}")
    return "; ".join(lines)


@butter.action("discord")
def discord(ctx: butter.Context) -> str:
    return roblox_rpc.start(ctx, required=True)


@butter.action("reinstall")
def reinstall(ctx: butter.Context) -> str:
    bottle = bottle_for(ctx, create=False)
    wanted = [c for c in COMPONENTS if rbx.is_installed(bottle, c)] or [PLAYER]
    for component in wanted:
        rbx.ensure(ctx, bottle, component, force=True)
        rbx.prepare(ctx, bottle, component)
    return "Reinstalled " + " and ".join(c.title for c in wanted)


@butter.action("mods")
def mods(ctx: butter.Context) -> str:
    for component in COMPONENTS:
        rbx.mods_dir(ctx, component).mkdir(parents=True, exist_ok=True)
    folder = ctx.plugin.data_dir / "mods"
    ctx.log(f"Mods for Roblox go in {folder / PLAYER.key}, and for Studio in {folder / STUDIO.key}.")
    ctx.log("Their files replace those at the same place in the app's Contents/Resources,")
    ctx.log("such as content/sounds/ouch.ogg. They are applied each time an app starts.")
    ctx.show_location(folder)
    return f"Mods go in {folder}"
