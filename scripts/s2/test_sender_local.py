"""island-sender end to end, without Taso: a real Island log and broker,
the real island-cli for approvals, and the stand-in Warden of
test_s2_flow.py for the sender socket.

Needs the built programs: ISLAND_BIN names their directory (default
target/debug); skipped if they are missing.  Run: `just s2-sender-test`,
or python3 -m unittest scripts/s2/test_sender_local.py after
`cargo build -p island-cli -p island-broker -p island-sender`.
"""

import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from test_s2_flow import VESSEL, StandIn, agent, report  # noqa: E402

BIN = os.environ.get("ISLAND_BIN", os.path.join(HERE, "..", "..", "target", "debug"))
PROGRAMS = ("island-cli", "island-broker", "island-sender")


def have_programs():
    return all(os.access(os.path.join(BIN, p), os.X_OK) for p in PROGRAMS)


def cli(*args):
    return subprocess.run([os.path.join(BIN, "island-cli"), *args],
                          check=True, capture_output=True, text=True).stdout.strip()


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@unittest.skipUnless(have_programs(), f"build {', '.join(PROGRAMS)} first (ISLAND_BIN={BIN})")
class Sender(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="s2-sender-")
        k = lambda name: os.path.join(self.tmp, name + ".key")  # noqa: E731
        for name in ("root", "appender", "approver", "sender"):
            cli("keygen", k(name))
        self.root = cli("public", k("root"))
        self.approver_key = k("approver")
        self.sender_key = k("sender")
        self.log = os.path.join(self.tmp, "island.log")
        cli("init", "--log", self.log, "--appender-key", k("appender"), "--root-key", k("root"))
        for who, right in (("approver", "approval"), ("sender", "dispatch")):
            cli("grant", "--log", self.log, "--appender-key", k("appender"), "--root-key", k("root"),
                "--holder", cli("public", k(who)), "--scope", "approvals", "--rights", right)
        self.appender_key = k("appender")
        self.broker_err = open(os.path.join(self.tmp, "broker.err"), "a")
        self.addCleanup(self.broker_err.close)
        self.port = free_port()
        self.broker_url = f"http://127.0.0.1:{self.port}"
        self.broker = None
        self.start_broker()
        self.work = os.path.join(self.tmp, "work")
        os.mkdir(self.work)

    def start_broker(self):
        self.broker = subprocess.Popen(
            [os.path.join(BIN, "island-broker"), "--log", self.log, "--appender-key", self.appender_key,
             "--root", self.root, "--listen", f"127.0.0.1:{self.port}"],
            stderr=self.broker_err)
        for _ in range(200):
            try:
                socket.create_connection(("127.0.0.1", self.port), timeout=0.2).close()
                return
            except OSError:
                time.sleep(0.05)
        raise RuntimeError("the broker did not start")

    def stop_broker(self):
        self.broker.terminate()
        self.broker.wait(10)

    def tearDown(self):
        self.stop_broker()

    def sender(self, standin, *extra, code=0, senders=False):
        where = ["--senders", os.path.dirname(standin.sender_path)] if senders else \
            ["--socket", standin.sender_path]
        r = subprocess.run([os.path.join(BIN, "island-sender"), *where,
                            "--broker", self.broker_url, "--root", self.root, "--key", self.sender_key,
                            "--once", "--wait", "5",
                            *extra], capture_output=True, text=True)
        self.assertEqual(r.returncode, code, r.stderr)
        return [json.loads(line) for line in r.stdout.splitlines() if line.strip()]

    def approve(self, request_hash, decline=False):
        cli("approve", "--broker", self.broker_url, "--root", self.root, "--key", self.approver_key,
            request_hash, *(["--decline"] if decline else []))

    def test_approved_once_declined_never(self):
        s = StandIn(self.tmp)
        p = agent(s, self.work, ["effect:irr", "effect:irr", "seal", "report", "wait:sender-done"])
        report(self.work)
        first = self.sender(s)
        self.assertEqual([x["action"] for x in first], ["wait", "wait"])
        self.approve(first[0]["request_hash"])
        self.approve(first[1]["request_hash"], decline=True)
        second = self.sender(s)
        self.assertEqual([(x["op"], x["action"], x.get("reported")) for x in second],
                         [(first[0]["op"], "send", True), (first[1]["op"], "decline", True)])
        self.assertEqual(self.sender(s), [], "nothing is left on the Open list")
        open(os.path.join(self.work, "sender-done"), "w").close()
        self.assertEqual(p.wait(10), 0)
        # The log holds one dispatch, for the approved op.
        tail = cli("tail", "--log", self.log, "--root", self.root, "-n", "20")
        self.assertEqual(tail.count(f"vessel {VESSEL} op {first[0]['op']} "), 1, tail)
        kinds = [line.split()[2] for line in tail.splitlines() if len(line.split()) > 2]
        self.assertEqual(kinds.count("dispatch"), 1, tail)

    def test_the_senders_directory_is_served(self):
        # The node's way: every <vessel>.sock in the directory, one pass.
        s = StandIn(self.tmp)
        p = agent(s, self.work, ["effect:irr", "seal", "report", "wait:sender-done"])
        report(self.work)
        first = self.sender(s, senders=True)
        self.assertEqual([(x["vessel"], x["action"]) for x in first], [(VESSEL, "wait")])
        self.approve(first[0]["request_hash"])
        second = self.sender(s, senders=True)
        self.assertEqual([(x["action"], x.get("reported")) for x in second], [("send", True)])
        self.assertEqual(self.sender(s, senders=True), [])
        open(os.path.join(self.work, "sender-done"), "w").close()
        self.assertEqual(p.wait(10), 0)

    def test_a_long_running_sender_outlives_a_broker_restart(self):
        s = StandIn(self.tmp)
        p = agent(s, self.work, ["effect:irr", "effect:irr", "seal", "report", "wait:sender-done"])
        report(self.work)
        out = open(os.path.join(self.tmp, "service.out"), "w+")
        self.addCleanup(out.close)
        err = open(os.path.join(self.tmp, "service.err"), "w+")
        self.addCleanup(err.close)
        service = subprocess.Popen([os.path.join(BIN, "island-sender"), "--senders", os.path.dirname(s.sender_path),
                                    "--broker", self.broker_url, "--root", self.root, "--key", self.sender_key,
                                    "--interval-ms", "100"], stdout=out, stderr=err, text=True)
        self.addCleanup(service.wait, 10)
        self.addCleanup(service.kill)

        def lines():
            out.flush()
            with open(out.name) as fh:
                return [json.loads(line) for line in fh if line.strip()]

        def wait_for(pred, what):
            for _ in range(200):
                got = lines()
                if pred(got):
                    return got
                time.sleep(0.05)
            self.fail(f"{what}: {lines()}")

        waiting = wait_for(lambda ls: len(ls) >= 2, "both effects listed")
        self.assertEqual([x["action"] for x in waiting], ["wait", "wait"], "one line each, not one per pass")
        self.approve(waiting[0]["request_hash"])
        wait_for(lambda ls: any(x["action"] == "send" for x in ls), "the first sent")
        self.stop_broker()
        time.sleep(0.5)
        self.start_broker()
        self.approve(waiting[1]["request_hash"])
        got = wait_for(lambda ls: sum(x["action"] == "send" for x in ls) == 2, "the second sent after the restart")
        self.assertIsNone(service.poll(), "the sender is still running")
        self.assertEqual([x["op"] for x in got if x["action"] == "send"], [waiting[0]["op"], waiting[1]["op"]])
        open(os.path.join(self.work, "sender-done"), "w").close()
        self.assertEqual(p.wait(10), 0)

    def test_crash_after_send_does_not_send_twice_and_unapproved_lapses(self):
        s = StandIn(self.tmp)
        p = agent(s, self.work, ["effect:irr", "effect:irr", "seal", "report", "stall"])
        report(self.work)
        first = self.sender(s)
        self.approve(first[0]["request_hash"])
        crashed = self.sender(s, "--stop-after-send", code=3)
        self.assertEqual([(x["action"], x.get("stopped_before_report")) for x in crashed], [("send", True)])
        p.kill()
        p.wait(10)
        s.crash()              # the Warden's restart: both released ops are unknown now
        self.stop_broker()     # and the broker restarts too
        self.start_broker()
        after = self.sender(s)
        self.assertEqual([(x["op"], x["action"]) for x in after],
                         [(first[0]["op"], "already-sent"), (first[1]["op"], "lapse")])
        self.assertEqual(after[0]["sent_at"], crashed[0]["sent_at"], "the same dispatch, not a second send")
        self.assertTrue(all(x["reported"] for x in after))
        self.assertTrue(s.authority, "both barriers settled: effect authority is back")


if __name__ == "__main__":
    unittest.main()
