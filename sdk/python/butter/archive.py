"""Unpacking archives the way a Mac would.

Python's own ``zipfile.extractall`` loses what Mac application bundles depend
on: symbolic links (``Versions/Current`` in every framework) become small
text files, and executables lose their execute bit. :func:`extract_zip`
keeps both, and refuses entries that would land outside the destination.
"""

from __future__ import annotations

import os
import shutil
import stat
import zipfile
from pathlib import Path
from typing import Callable, Optional

__all__ = ["extract_zip", "UnsafeArchive"]


class UnsafeArchive(Exception):
    """An entry would be written outside the destination folder."""


def _inside(root: str, path: str) -> bool:
    return path == root or path.startswith(root.rstrip(os.sep) + os.sep)


def extract_zip(
    archive: os.PathLike,
    dest: os.PathLike,
    progress: Optional[Callable[[float], None]] = None,
    skip_resource_forks: bool = True,
) -> Path:
    """Unpack the zip file ``archive`` into ``dest``, keeping symbolic links
    and permissions. ``progress`` is called with the fraction unpacked.
    ``__MACOSX/`` resource forks are left out unless ``skip_resource_forks``
    is false. Returns ``dest``."""
    dest = Path(dest)
    dest.mkdir(parents=True, exist_ok=True)
    root = os.path.realpath(dest)
    with zipfile.ZipFile(archive) as zf:
        entries = zf.infolist()
        total = sum(e.file_size for e in entries) or 1
        done = 0
        last = -1.0
        for entry in entries:
            name = entry.filename
            if skip_resource_forks and (
                name.startswith("__MACOSX/") or "/._" in "/" + name
            ):
                continue
            parts = [p for p in name.replace("\\", "/").split("/") if p not in ("", ".")]
            if not parts or ".." in parts or os.path.isabs(name):
                raise UnsafeArchive(f"unsafe entry {name!r}")
            target = os.path.join(root, *parts)
            # Writing through a link the archive made earlier could escape.
            # realpath resolves the folders that exist and keeps the rest, so
            # this is checked before any folder is made.
            parent = os.path.dirname(target)
            if not _inside(root, os.path.realpath(parent)):
                raise UnsafeArchive(f"{name!r} would be written outside {dest}")
            os.makedirs(parent, exist_ok=True)

            mode = (entry.external_attr >> 16) & 0xFFFF
            if stat.S_ISLNK(mode):
                link = zf.read(entry).decode("utf-8")
                _remove(target)
                os.symlink(link, target)
            elif entry.is_dir() or stat.S_ISDIR(mode):
                if os.path.islink(target):
                    raise UnsafeArchive(f"{name!r} is a link and a folder")
                os.makedirs(target, exist_ok=True)
            else:
                _remove(target)
                with zf.open(entry) as src, open(target, "wb") as out:
                    shutil.copyfileobj(src, out, 1024 * 1024)
                permissions = stat.S_IMODE(mode)
                os.chmod(target, permissions if permissions else 0o644)
            done += entry.file_size
            if progress:
                fraction = done / total
                if fraction - last >= 0.01:
                    progress(fraction)
                    last = fraction
    if progress:
        progress(1.0)
    return dest


def _remove(path: str) -> None:
    if os.path.islink(path) or os.path.isfile(path):
        os.unlink(path)
    elif os.path.isdir(path):
        shutil.rmtree(path)
