"""Tests of the Roblox plugin, with Butter and Roblox's servers faked.

Run with ``python3 -m unittest discover -s plugins/roblox/tests``.
"""

import json
import os
import shutil
import stat
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parents[2] / "sdk" / "python"))

import butter  # noqa: E402
import plugin  # noqa: E402
import roblox_deploy as deploy  # noqa: E402
import roblox_install as rbx  # noqa: E402


class DeployTests(unittest.TestCase):
    def test_channels_and_versions(self):
        self.assertEqual(deploy.normalize_channel(""), "LIVE")
        self.assertEqual(deploy.normalize_channel("live"), "LIVE")
        self.assertEqual(deploy.normalize_channel(" ZFlag "), "zflag")
        with self.assertRaises(deploy.DeployError):
            deploy.normalize_channel("../x")
        self.assertEqual(deploy.normalize_version("0123456789ABCDEF"), "version-0123456789abcdef")
        with self.assertRaises(deploy.DeployError):
            deploy.normalize_version("version-xyz")

    def test_urls(self):
        self.assertEqual(
            deploy.version_url(deploy.PLAYER, "LIVE"),
            "https://clientsettingscdn.roblox.com/v2/client-version/MacPlayer",
        )
        self.assertEqual(
            deploy.version_url(deploy.STUDIO, "zflag"),
            "https://clientsettingscdn.roblox.com/v2/client-version/MacStudio/channel/zflag",
        )
        self.assertEqual(
            deploy.package_url(deploy.PLAYER, "version-abc12345", "LIVE", deploy.X86_64),
            "https://setup.rbxcdn.com/mac/version-abc12345-RobloxPlayer.zip",
        )
        self.assertEqual(
            deploy.package_url(deploy.STUDIO, "version-abc12345", "zflag", deploy.ARM64),
            "https://setup.rbxcdn.com/channel/zflag/mac/arm64/version-abc12345-RobloxStudioApp.zip",
        )

    def test_architecture(self):
        self.assertEqual(deploy.choose_arch("auto", None, "x86_64"), deploy.X86_64)
        self.assertEqual(deploy.choose_arch("auto", None, "aarch64"), deploy.ARM64)
        self.assertEqual(deploy.choose_arch("auto", "x86_64", "aarch64"), deploy.X86_64)
        self.assertEqual(deploy.choose_arch("arm64", "x86_64", "x86_64"), deploy.ARM64)

    def test_links(self):
        link = "roblox-player:1+launchmode:play+gameinfo:T+placelauncherurl:https://x/y?a=1+channel:ZFlag"
        fields = deploy.parse_link(link)
        self.assertEqual(fields["roblox-player"], "1")
        self.assertEqual(fields["launchmode"], "play")
        self.assertEqual(fields["placelauncherurl"], "https://x/y?a=1")
        self.assertEqual(deploy.link_channel([link]), "zflag")
        self.assertIsNone(deploy.link_channel(["roblox://placeId=1"]))
        self.assertIs(deploy.component_for_link("roblox://placeId=1"), deploy.PLAYER)
        self.assertIs(deploy.component_for_link("roblox-studio-auth:x"), deploy.STUDIO)
        self.assertIsNone(deploy.component_for_link("/home/u/a.rbxl"))


def roblox_zip(path: Path, app: str, marker: str) -> Path:
    """A zip shaped like Roblox's: an app with an executable and a framework."""
    with zipfile.ZipFile(path, "w") as zf:

        def add(name, data, mode=stat.S_IFREG | 0o644):
            info = zipfile.ZipInfo(name)
            info.external_attr = mode << 16
            zf.writestr(info, data)

        add(f"{app}/Contents/Info.plist", "<plist/>")
        add(f"{app}/Contents/MacOS/{app[:-4]}", marker, stat.S_IFREG | 0o755)
        add(f"{app}/Contents/Resources/content/sounds/ouch.ogg", "original ouch")
        add(f"{app}/Contents/Frameworks/R.framework/Versions/A/R", "lib")
        add(f"{app}/Contents/Frameworks/R.framework/Versions/Current", "A", stat.S_IFLNK | 0o777)
    return path


class Fake:
    """Butter and Roblox's servers."""

    def __init__(self, root: Path):
        self.root = root
        self.bottles = {}
        self.settings = {}
        self.versions = {"MacPlayer": "version-1111111111111111", "MacStudio": "version-2222222222222222"}
        self.unavailable_channels = set()
        self.offline = False
        self.downloads = []
        self.created = []

    def add_bottle(self, bottle_id, name, ready=True):
        drive = self.root / bottle_id / "prefix" / "root"
        drive.mkdir(parents=True)
        self.bottles[bottle_id] = {
            "id": bottle_id,
            "name": name,
            "dir": str(self.root / bottle_id),
            "prefix": str(drive.parent),
            "drive": str(drive),
            "runner": "r",
            "arch": None,
            "ready": ready,
        }
        return self.bottles[bottle_id]

    # The calls a plugin makes.

    def call(self, method, params):
        if method == "bottles.list":
            return list(self.bottles.values())
        if method == "bottles.get":
            return self.bottles.get(params["id"])
        if method == "bottles.create":
            self.created.append(params["name"])
            return self.add_bottle(params["name"].lower(), params["name"])
        if method == "settings.set":
            self.settings[params["key"]] = params["value"]
            return None
        if method == "http.get":
            if self.offline:
                raise butter.PluginError("offline")
            url = params["url"]
            channel = url.split("/channel/")[1] if "/channel/" in url else None
            if channel in self.unavailable_channels:
                raise butter.PluginError("403 Forbidden")
            binary = url.split("/client-version/")[1].split("/")[0]
            version = self.versions[binary] if channel is None else "version-5555555555555555"
            return {"text": json.dumps({"clientVersionUpload": version})}
        if method == "http.download":
            url = params["url"]
            self.downloads.append(url)
            version = url.rsplit("/", 1)[1].split("-Roblox")[0]
            app = "RobloxStudio.app" if "Studio" in url else "RobloxPlayer.app"
            Path(params["dest"]).parent.mkdir(parents=True, exist_ok=True)
            roblox_zip(Path(params["dest"]), app, version)
            return {"sha256": "x"}
        if method == "apps.info":
            return {"icon": None}
        raise butter.PluginError(f"no call {method}")


class Channel:
    def __init__(self, fake):
        self.fake = fake
        self.sent = []

    def send(self, message):
        self.sent.append(message)

    def call(self, method, params):
        return self.fake.call(method, params)


class PluginTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.fake = Fake(self.root / "bottles")
        self.settings = {
            "bottle": None,
            "studio": False,
            "channel": "LIVE",
            "follow_link_channel": False,
            "version": "",
            "architecture": "auto",
            "update_on_launch": True,
            "renderer": "auto",
            "fflags_player": "{}",
            "fflags_studio": "{}",
            "mods": True,
        }

    def tearDown(self):
        self.tmp.cleanup()

    def ctx(self, action, open=(), bottle=None, **settings):
        values = dict(self.settings)
        values.update(self.fake.settings)
        values.update(settings)
        request = {
            "action": action,
            "plugin": {
                "id": "roblox",
                "name": "Roblox",
                "dir": str(HERE.parent),
                "data_dir": str(self.root / "data"),
                "cache_dir": str(self.root / "cache"),
            },
            "settings": values,
            "open": list(open),
            "bottle": bottle,
            "host": {"arch": "x86_64"},
        }
        return butter.Context(request, Channel(self.fake))

    def app(self, bottle_id, name="Roblox.app"):
        return Path(self.fake.bottles[bottle_id]["drive"]) / "Applications" / name

    def test_setup_makes_a_bottle_and_shortcut(self):
        ctx = self.ctx("setup")
        message = butter._actions["setup"](ctx)
        self.assertIn("Roblox installed", message)
        self.assertEqual(self.fake.created, ["Roblox"])
        self.assertEqual(self.fake.settings["bottle"], "roblox")
        app = self.app("roblox")
        self.assertTrue(os.access(app / "Contents/MacOS/RobloxPlayer", os.X_OK))
        self.assertTrue((app / "Contents/Frameworks/R.framework/Versions/Current").is_symlink())
        self.assertEqual(
            self.fake.downloads,
            ["https://setup.rbxcdn.com/mac/version-1111111111111111-RobloxPlayer.zip"],
        )
        shortcuts = [e for e in ctx.effects if e["type"] == "shortcut"]
        self.assertEqual(len(shortcuts), 1)
        self.assertEqual(shortcuts[0]["action"], "player")
        self.assertEqual(shortcuts[0]["schemes"], ["roblox-player", "roblox"])
        # Nothing is left behind.
        leftovers = [p.name for p in app.parent.iterdir() if p.name.startswith(".")]
        self.assertEqual(leftovers, [])
        self.assertEqual(list((self.root / "cache" / "downloads").iterdir()), [])

    def test_existing_bottle_named_roblox_is_used(self):
        self.fake.add_bottle("games", "roblox")
        butter._actions["setup"](self.ctx("setup"))
        self.assertEqual(self.fake.created, [])
        self.assertEqual(self.fake.settings["bottle"], "games")

    def test_launch_updates_only_when_needed(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        link = "roblox-player:1+launchmode:play+gameinfo:T"
        ctx = self.ctx("player", open=[link, "roblox-studio:1"])
        self.assertEqual(butter._actions["player"](ctx), "Starting Roblox")
        launch = [e for e in ctx.effects if e["type"] == "launch"][0]
        self.assertEqual(launch["bottle"], "rb")
        self.assertEqual(launch["target"], str(self.app("rb")))
        self.assertEqual(launch["open"], [link], "links for Studio are not passed")
        self.assertEqual(len(self.fake.downloads), 1)

        butter._actions["player"](self.ctx("player"))
        self.assertEqual(len(self.fake.downloads), 1, "up to date")

        self.fake.versions["MacPlayer"] = "version-3333333333333333"
        butter._actions["player"](self.ctx("player"))
        self.assertEqual(len(self.fake.downloads), 2)
        self.assertEqual(
            (self.app("rb") / "Contents/MacOS/RobloxPlayer").read_text(),
            "version-3333333333333333",
        )

        self.fake.versions["MacPlayer"] = "version-4444444444444444"
        butter._actions["player"](self.ctx("player", update_on_launch=False))
        self.assertEqual(len(self.fake.downloads), 2, "updates are off")

        self.fake.offline = True
        ctx = self.ctx("player")
        butter._actions["player"](ctx)
        self.assertTrue(any(e["type"] == "launch" for e in ctx.effects), "offline, it still starts")

    def test_channels(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        link = "roblox-player:1+launchmode:play+channel:zbeta"
        butter._actions["player"](self.ctx("player", open=[link]))
        self.assertNotIn("zbeta", self.fake.downloads[-1], "links don't choose by default")

        butter._actions["player"](self.ctx("player", open=[link], follow_link_channel=True))
        self.assertEqual(
            self.fake.downloads[-1],
            "https://setup.rbxcdn.com/channel/zbeta/mac/version-5555555555555555-RobloxPlayer.zip",
        )

        self.fake.unavailable_channels.add("private")
        butter._actions["reinstall"](self.ctx("reinstall", channel="Private"))
        self.assertTrue(self.fake.downloads[-1].startswith("https://setup.rbxcdn.com/mac/"))

    def test_pinned_version_and_arch(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        butter._actions["player"](
            self.ctx("player", version="abcdefabcdefabcd", architecture="arm64")
        )
        self.assertEqual(
            self.fake.downloads[-1],
            "https://setup.rbxcdn.com/mac/arm64/version-abcdefabcdefabcd-RobloxPlayer.zip",
        )
        with self.assertRaises(butter.PluginError):
            butter._actions["player"](self.ctx("player", version="nope!"))

    def test_fflags(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        butter._actions["player"](
            self.ctx("player", renderer="metal", fflags_player='{"FFlagX": "True"}')
        )
        flags = json.loads((self.app("rb") / rbx.FFLAGS_FILE).read_text())
        self.assertEqual(flags, {"FFlagDebugGraphicsPreferMetal": "True", "FFlagX": "True"})
        butter._actions["player"](self.ctx("player"))
        self.assertFalse((self.app("rb") / rbx.FFLAGS_FILE).exists())
        with self.assertRaises(butter.PluginError):
            butter._actions["player"](self.ctx("player", fflags_player="[1"))

    def test_mods_are_applied_and_undone(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        mods = self.root / "data" / "mods" / "player" / "content" / "sounds"
        mods.mkdir(parents=True)
        (mods / "ouch.ogg").write_text("modded ouch")
        (mods / "new.ogg").write_text("new")
        butter._actions["player"](self.ctx("player"))
        sounds = self.app("rb") / "Contents/Resources/content/sounds"
        self.assertEqual((sounds / "ouch.ogg").read_text(), "modded ouch")
        self.assertEqual((sounds / "new.ogg").read_text(), "new")

        (mods / "ouch.ogg").unlink()
        butter._actions["player"](self.ctx("player"))
        self.assertEqual((sounds / "ouch.ogg").read_text(), "original ouch")
        self.assertTrue((sounds / "new.ogg").exists())

        butter._actions["player"](self.ctx("player", mods=False))
        self.assertFalse((sounds / "new.ogg").exists())

        ctx = self.ctx("mods")
        butter._actions["mods"](ctx)
        self.assertEqual(ctx.effects[-1]["type"], "open")

    def test_studio_and_bottle_actions(self):
        bottle = self.fake.add_bottle("games", "Games")
        ctx = self.ctx("install-here", bottle=bottle, studio=True)
        butter._actions["install-here"](ctx)
        self.assertEqual(self.fake.settings["bottle"], "games")
        self.assertTrue(self.app("games", "RobloxStudio.app").is_dir())
        actions = [e["action"] for e in ctx.effects if e["type"] == "shortcut"]
        self.assertEqual(actions, ["player", "studio"])

        ctx = self.ctx("studio", open=["roblox-studio-auth:code=1"])
        butter._actions["studio"](ctx)
        launch = [e for e in ctx.effects if e["type"] == "launch"][0]
        self.assertTrue(launch["target"].endswith("RobloxStudio.app"))
        self.assertEqual(launch["open"], ["roblox-studio-auth:code=1"])

        message = butter._actions["update"](self.ctx("update"))
        self.assertIn("Roblox Studio is up to date", message)

    def test_update_needs_setup(self):
        with self.assertRaises(butter.PluginError):
            butter._actions["update"](self.ctx("update"))

    def test_unready_bottle_is_explained(self):
        self.fake.add_bottle("rb", "Roblox", ready=False)
        self.fake.settings["bottle"] = "rb"
        with self.assertRaises(butter.PluginError) as e:
            butter._actions["player"](self.ctx("player"))
        self.assertIn("no prefix", str(e.exception))


if __name__ == "__main__":
    unittest.main()
