"""Tests of the Roblox plugin, with Butter and Roblox's servers faked.

Run with ``python3 -m unittest discover -s plugins/roblox/tests``.
"""

import json
import os
import plistlib
import shutil
import stat
import subprocess
import sys
import tempfile
import time
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
import roblox_rpc  # noqa: E402


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

    def test_plist_channel(self):
        xml = plistlib.dumps({"www.roblox.com": "ZFlag"})
        self.assertEqual(deploy.plist_channel(xml), "zflag")
        binary = plistlib.dumps({"www.roblox.com": "production"}, fmt=plistlib.FMT_BINARY)
        self.assertEqual(deploy.plist_channel(binary), "LIVE")
        self.assertEqual(deploy.plist_channel(plistlib.dumps({"other": "zbeta"})), "zbeta")
        self.assertIsNone(deploy.plist_channel(plistlib.dumps({"www.roblox.com": ""})))
        self.assertIsNone(deploy.plist_channel(plistlib.dumps({"www.roblox.com": "../x"})))
        self.assertIsNone(deploy.plist_channel(b"not a plist"))


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
        if app == "RobloxPlayer.app":
            add(
                f"{app}/Contents/MacOS/RobloxPlayerInstaller.app/Contents/MacOS/RobloxPlayerInstaller",
                "installer",
                stat.S_IFREG | 0o755,
            )
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
            # On in plugin.toml; off here so launching never starts a real
            # roserpc.
            "discord_rpc": False,
            "roserpc_command": "",
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

    def enroll(self, bottle_id, channel, user="me"):
        prefs = Path(self.fake.bottles[bottle_id]["drive"]) / "Users" / user / "Library" / "Preferences"
        prefs.mkdir(parents=True, exist_ok=True)
        (prefs / "com.roblox.RobloxPlayerChannel.plist").write_bytes(
            plistlib.dumps({"www.roblox.com": channel})
        )

    def test_enrolled_channel(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        butter._actions["player"](self.ctx("player"))
        self.assertEqual(
            self.fake.downloads[-1],
            "https://setup.rbxcdn.com/mac/version-1111111111111111-RobloxPlayer.zip",
        )

        # Roblox enrolls the user; the next launch switches to that channel.
        self.enroll("rb", "zcanary")
        butter._actions["player"](self.ctx("player"))
        self.assertEqual(
            self.fake.downloads[-1],
            "https://setup.rbxcdn.com/channel/zcanary/mac/version-5555555555555555-RobloxPlayer.zip",
        )
        ctx = self.ctx("player")
        entry = rbx.record(ctx, plugin.bottle_for(ctx), deploy.PLAYER)
        self.assertEqual(entry["channel"], "zcanary")
        count = len(self.fake.downloads)
        butter._actions["player"](self.ctx("player"))
        self.assertEqual(len(self.fake.downloads), count, "up to date on the enrolled channel")

        # A channel in the settings wins, and the enrolled one can be ignored.
        butter._actions["player"](self.ctx("player", channel="zother"))
        self.assertIn("/channel/zother/", self.fake.downloads[-1])
        butter._actions["player"](self.ctx("player", follow_enrolled_channel=False))
        self.assertTrue(self.fake.downloads[-1].startswith("https://setup.rbxcdn.com/mac/"))

        # Back to production: back to LIVE.
        self.enroll("rb", "production")
        butter._actions["player"](self.ctx("player", channel="zother"))
        butter._actions["player"](self.ctx("player"))
        self.assertTrue(self.fake.downloads[-1].startswith("https://setup.rbxcdn.com/mac/"))

    def test_installer_is_removed(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        butter._actions["player"](self.ctx("player"))
        installer = self.app("rb") / "Contents/MacOS/RobloxPlayerInstaller.app"
        self.assertFalse(installer.exists())
        self.assertTrue((self.app("rb") / "Contents/MacOS/RobloxPlayer").exists())

        # One that comes back is removed again before Roblox starts.
        (installer / "Contents").mkdir(parents=True)
        butter._actions["player"](self.ctx("player"))
        self.assertFalse(installer.exists())

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

    def test_discord_rich_presence_is_on_by_default(self):
        manifest = (HERE.parent / "plugin.toml").read_text()
        setting = manifest.split('key = "discord_rpc"')[1].split("[[settings]]")[0]
        self.assertIn("default = true", setting)

    def test_discord_rich_presence_starts_with_the_player_only(self):
        self.fake.add_bottle("rb", "Roblox")
        self.fake.settings["bottle"] = "rb"
        started = []
        original = roblox_rpc.start
        roblox_rpc.start = lambda ctx, required=False: started.append(ctx.action)
        try:
            butter._actions["player"](self.ctx("player"))
            self.assertEqual(started, [])
            butter._actions["player"](self.ctx("player", discord_rpc=True))
            self.assertEqual(started, ["player"])
            butter._actions["studio"](self.ctx("studio", discord_rpc=True))
            self.assertEqual(started, ["player"])
        finally:
            roblox_rpc.start = original


class RoserpcTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.running = roblox_rpc.running
        roblox_rpc.running = lambda: []
        # Stands in for Butter, whose exit stops roserpc.
        self.butter = subprocess.Popen(["sleep", "60"])
        self.butter_pidfd = roblox_rpc.butter_pidfd
        roblox_rpc.butter_pidfd = lambda: os.pidfd_open(self.butter.pid)

    def tearDown(self):
        roblox_rpc.running = self.running
        roblox_rpc.butter_pidfd = self.butter_pidfd
        self.butter.kill()
        self.butter.wait()
        self.tmp.cleanup()

    def gone(self, pid, timeout=10.0):
        """Whether ``pid`` ends within ``timeout`` seconds."""
        deadline = time.monotonic() + timeout
        while True:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.05)

    def ctx(self, command):
        request = {
            "action": "discord",
            "plugin": {
                "id": "roblox",
                "name": "Roblox",
                "dir": str(HERE.parent),
                "data_dir": str(self.root / "data"),
                "cache_dir": str(self.root / "cache"),
            },
            "settings": {"roserpc_command": command},
            "host": {"arch": "x86_64"},
        }
        self.channel = Channel(Fake(self.root))
        return butter.Context(request, self.channel)

    def logged(self):
        return "\n".join(m["text"] for m in self.channel.sent if m["t"] == "log")

    def script(self, body):
        path = self.root / "fake roserpc"
        path.write_text("#!/bin/sh\n" + body)
        path.chmod(0o755)
        return path

    def test_recognises_roserpc_command_lines(self):
        self.assertTrue(roblox_rpc.is_roserpc([b"/home/u/.local/bin/roserpc"]))
        self.assertTrue(roblox_rpc.is_roserpc([b"python3.12", b"-I", b"-m", b"roserpc", b"-v"]))
        self.assertFalse(roblox_rpc.is_roserpc([b"vim", b"/home/u/roserpc"]))
        self.assertFalse(roblox_rpc.is_roserpc([b"python3", b"-m", b"roserpcx"]))
        self.assertFalse(roblox_rpc.is_roserpc([]))

    def test_finds_running_roserpc_in_proc(self):
        proc = self.root / "proc"
        for pid, cmdline in [("10", b"/x/roserpc\0"), ("11", b"bash\0"), ("self", b"")]:
            (proc / pid).mkdir(parents=True)
            (proc / pid / "cmdline").write_bytes(cmdline)
        self.assertEqual(self.running(proc), [10])

    def test_configured_command_is_split_and_expanded(self):
        self.assertEqual(
            roblox_rpc.find_command("~/rpc/bin/roserpc --hide-menu"),
            [str(Path("~/rpc/bin/roserpc").expanduser()), "--hide-menu"],
        )

    def start_sleeper(self):
        """Start a fake roserpc that runs until stopped; returns its pid."""
        pidfile = self.root / "pid"
        script = self.script(f"echo $$ > '{pidfile}'\necho started\nexec sleep 30\n")
        message = roblox_rpc.start(self.ctx(f"'{script}' -v"), required=True)
        self.assertEqual(message, "Discord Rich Presence is on")
        pid = int(pidfile.read_text())
        self.addCleanup(lambda: self.gone(pid, 0) or os.kill(pid, 9))
        return pid

    def test_starts_roserpc_outside_the_action(self):
        pid = self.start_sleeper()
        self.assertNotEqual(os.getsid(pid), os.getsid(0))
        log = (self.root / "data" / roblox_rpc.LOG_NAME).read_text()
        self.assertIn("started", log)
        self.assertFalse(self.gone(pid, 0.3))

    def test_roserpc_stops_when_butter_exits(self):
        pid = self.start_sleeper()
        self.butter.kill()
        self.butter.wait()
        self.assertTrue(self.gone(pid))
        log = (self.root / "data" / roblox_rpc.LOG_NAME).read_text()
        self.assertIn("Butter exited; stopping roserpc", log)

    def test_without_pidfds_roserpc_runs_on_its_own(self):
        roblox_rpc.butter_pidfd = lambda: None
        pid = self.start_sleeper()
        self.assertEqual(os.getsid(pid), pid)
        self.assertIn("keep running after Butter quits", self.logged())

    def test_roserpc_that_exits_at_once_is_reported(self):
        script = self.script("echo 'roserpc needs Python 3.10'\nexit 3\n")
        with self.assertRaises(butter.PluginError) as e:
            roblox_rpc.start(self.ctx(f"'{script}'"), required=True)
        self.assertIn("exit code 3", str(e.exception))
        self.assertIn("needs Python 3.10", self.logged())
        # Starting with Roblox only logs it, so Roblox still starts.
        self.assertIsNone(roblox_rpc.start(self.ctx(f"'{script}'")))

    def test_already_running_roserpc_is_left_alone(self):
        roblox_rpc.running = lambda: [42]
        script = self.script(f"touch '{self.root / 'ran'}'\n")
        message = roblox_rpc.start(self.ctx(f"'{script}'"), required=True)
        self.assertEqual(message, "Discord Rich Presence is already on")
        self.assertFalse((self.root / "ran").exists())

    def test_missing_roserpc_is_explained(self):
        original = roblox_rpc.find_command
        roblox_rpc.find_command = lambda configured="": None
        try:
            with self.assertRaises(butter.PluginError) as e:
                roblox_rpc.start(self.ctx(""), required=True)
            self.assertIn("roserpc was not found", str(e.exception))
        finally:
            roblox_rpc.find_command = original


if __name__ == "__main__":
    unittest.main()
