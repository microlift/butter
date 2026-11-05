"""Tests of the butter module that need no Butter.

Run with ``python3 -m unittest discover -s sdk/python/tests``.
"""

import io
import json
import os
import stat
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import butter  # noqa: E402
from butter.archive import UnsafeArchive, extract_zip  # noqa: E402


def add(zf, name, data=b"", mode=0o644, link=None, directory=False):
    info = zipfile.ZipInfo(name)
    if link is not None:
        info.external_attr = (stat.S_IFLNK | 0o777) << 16
        data = link.encode()
    elif directory:
        info.external_attr = (stat.S_IFDIR | 0o755) << 16
    else:
        info.external_attr = (stat.S_IFREG | mode) << 16
    zf.writestr(info, data)


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def zip(self, build):
        path = self.root / "a.zip"
        with zipfile.ZipFile(path, "w") as zf:
            build(zf)
        return path

    def test_keeps_links_and_modes(self):
        def build(zf):
            add(zf, "Foo.app/", directory=True)
            add(zf, "Foo.app/Contents/MacOS/Foo", b"#!", mode=0o755)
            add(zf, "Foo.app/Contents/Frameworks/X.framework/Versions/A/X", b"lib")
            add(zf, "Foo.app/Contents/Frameworks/X.framework/Versions/Current", link="A")
            add(zf, "Foo.app/Contents/Frameworks/X.framework/X", link="Versions/Current/X")
            add(zf, "__MACOSX/Foo.app/._Foo", b"fork")

        seen = []
        out = extract_zip(self.zip(build), self.root / "out", progress=seen.append)
        app = out / "Foo.app"
        self.assertTrue(os.access(app / "Contents/MacOS/Foo", os.X_OK))
        current = app / "Contents/Frameworks/X.framework/Versions/Current"
        self.assertTrue(current.is_symlink())
        self.assertEqual(os.readlink(current), "A")
        self.assertEqual((app / "Contents/Frameworks/X.framework/X").read_bytes(), b"lib")
        self.assertFalse((out / "__MACOSX").exists())
        self.assertEqual(seen[-1], 1.0)

    def test_refuses_escapes(self):
        for build in (
            lambda zf: add(zf, "../evil", b"x"),
            lambda zf: add(zf, "/abs/evil", b"x"),
            lambda zf: (add(zf, "out", link="/tmp"), add(zf, "out/evil", b"x")),
            lambda zf: (add(zf, "up", link=".."), add(zf, "up/x/evil", b"x")),
        ):
            with self.subTest(build=build):
                with self.assertRaises(UnsafeArchive):
                    extract_zip(self.zip(build), self.root / "dest")
        self.assertFalse((self.root / "evil").exists())
        self.assertFalse((self.root / "x").exists())


class Channel:
    """Butter's side of the pipe, answering from a table."""

    def __init__(self, answers):
        self.sent = []
        self.answers = answers
        self.calls = []

    def send(self, message):
        self.sent.append(message)

    def call(self, method, params):
        self.calls.append((method, params))
        answer = self.answers[method]
        if isinstance(answer, Exception):
            raise answer
        return answer(params) if callable(answer) else answer


def request(**extra):
    req = {
        "action": "a",
        "plugin": {
            "id": "p",
            "name": "P",
            "dir": "/p",
            "data_dir": extra.pop("data_dir", "/data"),
            "cache_dir": "/cache",
        },
        "settings": {"x": 1, "none": None},
        "open": ["foo://bar"],
        "host": {"arch": "x86_64"},
    }
    req.update(extra)
    return req


class ContextTests(unittest.TestCase):
    def test_reports_and_effects(self):
        channel = Channel({"settings.set": None})
        ctx = butter.Context(request(), channel)
        ctx.stage("One", 0.5)
        ctx.log("a", 1)
        self.assertEqual(ctx.setting("x"), 1)
        self.assertEqual(ctx.setting("none", "default"), "default")
        ctx.set_setting("x", 2)
        self.assertEqual(ctx.setting("x"), 2)
        bottle = butter.Bottle(
            {"id": "b", "name": "B", "dir": "/b", "prefix": "/b/p", "drive": "/b/p/root"}
        )
        self.assertEqual(bottle.path("/Applications/X.app"), Path("/b/p/root/Applications/X.app"))
        ctx.launch(bottle, bottle.path("/Applications/X.app"), open=ctx.open)
        ctx.add_shortcut("X", "a", schemes=["foo"])
        ctx.notify("hi")
        self.assertEqual(channel.sent[0], {"t": "stage", "title": "One", "fraction": 0.5})
        self.assertEqual(channel.sent[1], {"t": "log", "text": "a 1"})
        self.assertEqual([e["type"] for e in ctx.effects], ["launch", "shortcut", "notify"])
        self.assertEqual(ctx.effects[0]["open"], ["foo://bar"])
        json.dumps(ctx.effects)

    def test_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            ctx = butter.Context(request(data_dir=tmp), Channel({}))
            self.assertEqual(ctx.load_state(), {})
            ctx.save_state({"a": [1]})
            self.assertEqual(ctx.load_state(), {"a": [1]})

    def test_channel_round_trip(self):
        reader = io.StringIO(
            json.dumps({"t": "reply", "id": 1, "ok": {"text": "hi"}})
            + "\n"
            + json.dumps({"t": "reply", "id": 2, "error": "nope"})
            + "\n"
        )
        writer = io.StringIO()
        channel = butter.Channel(reader, writer)
        ctx = butter.Context(request(), channel)
        self.assertEqual(ctx.http_get("https://x"), "hi")
        with self.assertRaises(butter.PluginError):
            ctx.http_get("https://y")
        sent = [json.loads(line) for line in writer.getvalue().splitlines()]
        self.assertEqual(sent[0]["method"], "http.get")
        self.assertEqual(sent[1]["id"], 2)


if __name__ == "__main__":
    unittest.main()
