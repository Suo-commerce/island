#!/usr/bin/env python3
"""The face on this machine, against a real session, without Taso.

Starts what `just agent-test` starts (island-broker on a throwaway log,
flare-server, island-socket, island-sender, a stand-in Warden, flare-agent
with the scripted model), with flare-server serving the built page, and
leaves it running until Ctrl-C. Nothing of the real Island is touched.

    just face-stack            then open the address it prints

The scripted model (scripts/s4/scripted_model.py) answers by rule: any
message is "written to hello.txt"; "propose a verse" makes an action that
waits; "fork" forks two children (as root only: a child needs a uid).
The terminal attaches to the same session with the command it prints;
`/approve N` there settles what the page shows waiting.
"""

import os
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
REPO = os.path.join(HERE, "..", "..")

os.environ.setdefault("FLARE_FACE", os.path.join(REPO, "face", "dist"))
os.environ.setdefault("FLARE_FACE_PORT", "4759")

import test_converse_local as stack  # noqa: E402


class Stack(stack.Converse):
    def runTest(self):
        pass


def stop(*_):
    raise KeyboardInterrupt


def main():
    signal.signal(signal.SIGTERM, stop)
    face = os.environ["FLARE_FACE"]
    if not os.path.isfile(os.path.join(face, "index.html")):
        sys.exit(f"{face}: no built page there; run `just face` first")
    running = Stack()
    try:
        running.setUp()
        if os.getuid() == 0:
            running.keep_the_prime_running()
        port = running.session_port
        print(f"\nthe face      http://127.0.0.1:{port}/", flush=True)
        print("the terminal  " + " ".join([
            os.path.join(stack.BIN, "flare-tui"), "--server", f"ws://127.0.0.1:{port}/session",
            "--session", "main", "--client", "laptop", "--root", running.root,
            "--broker", running.broker, "--replica", os.path.join(running.tmp, "replica.log"),
            "--key", running.daily]), flush=True)
        print("Ctrl-C stops it and removes its files.", flush=True)
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        # Whatever was started is stopped, also after a start that failed.
        if hasattr(running, "stopping"):
            running.stopping.set()   # the prime is not started again
            time.sleep(0.2)
        for process in reversed(list(getattr(running, "procs", {}).values())):
            if process.poll() is None:
                process.terminate()
        for process in getattr(running, "procs", {}).values():
            try:
                process.wait(10)
            except subprocess.TimeoutExpired:
                process.kill()
        running.doCleanups()
        shutil.rmtree(getattr(running, "tmp", ""), ignore_errors=True)

if __name__ == "__main__":
    main()
