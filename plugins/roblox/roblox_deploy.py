"""Where Roblox's Mac builds come from.

Roblox publishes each build of the Mac app as one zip file on its setup CDN:

* the newest version of a channel comes from the client settings service,
  ``https://clientsettingscdn.roblox.com/v2/client-version/MacPlayer``
  (``MacStudio`` for Studio), with ``/channel/<name>`` added for a channel
  other than LIVE. Its ``clientVersionUpload`` is the version, such as
  ``version-0123456789abcdef``;
* the zip is ``https://setup.rbxcdn.com/mac/<version>-RobloxPlayer.zip``
  (``RobloxStudioApp.zip`` for Studio), under ``mac/arm64/`` for the ARM
  build, and under ``channel/<name>/`` for a channel other than LIVE.

Links from the website look like ``roblox-player:1+launchmode:play+
gameinfo:...+channel:zflag``: ``+``-separated ``key:value`` pairs.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Dict, Optional, Tuple

CLIENT_SETTINGS = "https://clientsettingscdn.roblox.com"
SETUP = "https://setup.rbxcdn.com"
LIVE = "LIVE"

ARM64 = "arm64"
X86_64 = "x86-64"


@dataclass(frozen=True)
class Component:
    """One of the two apps."""

    #: The plugin's action and the key of its records.
    key: str
    title: str
    #: What the client settings service calls it.
    binary_type: str
    #: The file on the setup CDN, after ``<version>-``.
    package: str
    #: What it is installed as, in ``/Applications``.
    app_name: str
    #: The links it opens.
    schemes: Tuple[str, ...]
    #: Its FastFlags setting.
    fflags_setting: str
    #: The freedesktop category of its shortcut.
    categories: str


PLAYER = Component(
    key="player",
    title="Roblox",
    binary_type="MacPlayer",
    package="RobloxPlayer.zip",
    app_name="Roblox.app",
    schemes=("roblox-player", "roblox"),
    fflags_setting="fflags_player",
    categories="Game;",
)

STUDIO = Component(
    key="studio",
    title="Roblox Studio",
    binary_type="MacStudio",
    package="RobloxStudioApp.zip",
    app_name="RobloxStudio.app",
    schemes=("roblox-studio", "roblox-studio-auth"),
    fflags_setting="fflags_studio",
    categories="Development;",
)

COMPONENTS = (PLAYER, STUDIO)

_CHANNEL = re.compile(r"^[a-z0-9][a-z0-9_.-]{0,63}$")
_VERSION = re.compile(r"^version-[0-9a-f]{8,32}$")


class DeployError(ValueError):
    """A channel, version or link that can't be used."""


def normalize_channel(channel: Optional[str]) -> str:
    """``LIVE``, or a channel's name in lower case, as the CDN spells it."""
    channel = (channel or "").strip()
    if not channel or channel.lower() == "live" or channel.lower() == "production":
        return LIVE
    channel = channel.lower()
    if not _CHANNEL.match(channel):
        raise DeployError(f"{channel!r} is not a channel name")
    return channel


def normalize_version(version: str) -> str:
    """``version-<hex>``, from what a user typed, with or without the prefix."""
    version = version.strip().lower()
    if not version.startswith("version-"):
        version = "version-" + version
    if not _VERSION.match(version):
        raise DeployError(f"{version!r} is not a Roblox version, such as version-0123456789abcdef")
    return version


def version_url(component: Component, channel: str) -> str:
    """Where the newest version of ``component`` on ``channel`` is listed."""
    url = f"{CLIENT_SETTINGS}/v2/client-version/{component.binary_type}"
    if channel != LIVE:
        url += f"/channel/{channel}"
    return url


def package_url(component: Component, version: str, channel: str, arch: str) -> str:
    """The zip of ``component`` at ``version``."""
    root = SETUP if channel == LIVE else f"{SETUP}/channel/{channel}"
    folder = "/mac/arm64/" if arch == ARM64 else "/mac/"
    return f"{root}{folder}{version}-{component.package}"


def choose_arch(setting: Optional[str], bottle_arch: Optional[str], host_arch: str) -> str:
    """The build to install: the one chosen, or the bottle's architecture,
    which follows this computer's unless set."""
    if setting in (ARM64, X86_64):
        return setting
    arch = bottle_arch or host_arch
    return ARM64 if arch in ("aarch64", "arm64") else X86_64


def parse_link(link: str) -> Dict[str, str]:
    """The fields of a ``roblox-player:``/``roblox-studio:`` launch link.
    The scheme itself maps to the protocol version (usually ``1``).
    Links of other shapes, such as ``roblox://placeId=1``, give ``{}``
    beyond their scheme."""
    scheme, _, rest = link.partition(":")
    fields = {"scheme": scheme.lower()}
    if rest.startswith("//"):
        return fields
    parts = rest.split("+")
    if parts:
        fields[scheme.lower()] = parts[0]
    for part in parts[1:]:
        key, sep, value = part.partition(":")
        if sep:
            fields[key] = value
    return fields


def link_channel(links) -> Optional[str]:
    """The channel the first launch link names, if any."""
    for link in links:
        channel = parse_link(link).get("channel")
        if channel:
            try:
                return normalize_channel(channel)
            except DeployError:
                return None
    return None


def component_for_link(link: str) -> Optional[Component]:
    """The app a link is for."""
    scheme = link.partition(":")[0].lower()
    for component in COMPONENTS:
        if scheme in component.schemes:
            return component
    return None
