"""Runs roserpc for as long as Butter runs.

Usage: ``python3 -I roserpc_keeper.py BUTTER_PIDFD COMMAND...``

BUTTER_PIDFD is a pidfd of Butter's process, inherited from the plugin. When
it becomes readable Butter has exited, and roserpc is asked to stop (it
clears Discord's activity as it does). When roserpc ends on its own, so does
the keeper, with roserpc's exit code. A pidfd, not a pid, so a recycled pid
is never mistaken for Butter. Standard library only: it runs without the
butter module on its path.
"""

from __future__ import annotations

import os
import select
import signal
import subprocess
import sys
from typing import List

# roserpc closes its Discord connection on SIGTERM; one that hangs is killed.
STOP_GRACE = 5.0


def stop(child: subprocess.Popen) -> None:
    child.terminate()
    try:
        child.wait(timeout=STOP_GRACE)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()


def main(argv: List[str]) -> int:
    if len(argv) < 3:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    butter_fd = int(argv[1])
    child = subprocess.Popen(argv[2:], stdin=subprocess.DEVNULL)
    child_fd = os.pidfd_open(child.pid)
    # Stopping the keeper stops roserpc too; poll() is resumed afterwards
    # and sees roserpc end.
    signal.signal(signal.SIGTERM, lambda *_: child.terminate())
    signal.signal(signal.SIGINT, lambda *_: child.terminate())

    poller = select.poll()
    poller.register(butter_fd, select.POLLIN)
    poller.register(child_fd, select.POLLIN)
    while True:
        ready = {fd for fd, _ in poller.poll()}
        if child_fd in ready:
            return child.wait()
        if butter_fd in ready:
            print("Butter exited; stopping roserpc", file=sys.stderr, flush=True)
            stop(child)
            return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
