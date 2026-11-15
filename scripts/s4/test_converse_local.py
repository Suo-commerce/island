"""Steps 4 to 6 end to end, without Taso: a message said in the session
reaches flare-agent as a task, and its reply comes back to the session
once; a verse waits for approval; and, run as root, a task is forked two
ways, one child is promoted, and the agent goes on from that child's files.

The real programs: island-broker on a real log, flare-server with its
agent socket, island-socket with a scripted model, flare-agent, and
island-sender; a stand-in Warden (scripts/s2/test_s2_flow.py) serves the
Field and the sender socket, and flare-tui says the message. The caller of
every socket is this process: the Warden config puts its uid in
flare-home's slot, and flare-server takes tasks and replies from it.

The fork test needs root: a fork child is known to the island socket by
its own uid, so the stand-in Warden (ForkingWarden, below) runs each
child's flare-agent as the uid of its fork slot. It is skipped otherwise;
on the Island server, `just vessel-gate` runs the same with Taso.

Needs the built programs: ISLAND_BIN names their directory (default
target/debug); skipped if they are missing. Run: `just agent-test`.
"""

import http.client
import http.server
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "s2"))
import s2_field as f  # noqa: E402
from scripted_model import ScriptedModel  # noqa: E402
from test_s2_flow import StandIn, _b, _u8, _u64  # noqa: E402

BIN = os.environ.get("ISLAND_BIN", os.path.join(HERE, "..", "..", "target", "debug"))
PROGRAMS = ("island-cli", "island-broker", "island-socket", "island-sender", "flare-server",
            "flare-agent", "flare-tui")
RANGE = 65536
FORK_SLOTS = [90, 91]


class ForkingWarden(StandIn):
    """A Vessel's Field and sender sockets, with the part of Taso forking
    needs: every Seal keeps a copy of the work directory; a fork starts a
    child (the same program, as the uid of the next fork slot) on a copy
    of the newest Seal, with every action of its held; a promotion puts
    the child's newest Seal in place of the prime's files and releases the
    child's held actions as the prime's, unknown and without effect
    authority until a sender settles them (spike S4, docs/slice-2.md)."""

    def __init__(self, tmp, work, world=None, hold=False, last_sealed=None):
        self.work = work
        self.world = world
        self.held = []
        self.seals = os.path.join(tmp, "seals")
        self.sealed_dir = None
        os.makedirs(self.seals, exist_ok=True)
        super().__init__(tmp, hold=hold, last_sealed=last_sealed)

    def _answer(self, body):
        r = f.Reader(body)
        tag = r.u8()
        if tag == 1 and self.hold:
            call = r.u64()
            provider, request = r.string(), r.raw()
            r.raw()
            assert r.u8() == 0
            key = r.raw()
            consequence = r.consequence()
            with self.lock:
                op, self.next_op = self.next_op, self.next_op + 1
                self.held.append((provider, request, key, consequence))
            return _u8(1) + _u64(call) + _u8(1) + _u64(op)
        if tag == 3:
            reply = super()._answer(body)
            with self.lock:
                kept = os.path.join(self.seals, str(self.last_sealed))
                shutil.rmtree(kept, ignore_errors=True)
                shutil.copytree(self.work, kept, symlinks=True)
                self.sealed_dir = kept
            return reply
        if tag == 6:
            r.opt_u64()
            made = self.world.fork(self)
            if isinstance(made, str):
                return _u8(7) + _u8(1) + _b(made.encode())
            name, seal = made
            return _u8(7) + _u8(0) + _b(name.encode()) + _u64(seal)
        if tag == 7:
            child = r.string()
            r.opt_u64()
            done = self.world.promote(self, child)
            if isinstance(done, str):
                return _u8(8) + _b(child.encode()) + _u8(1) + _b(done.encode())
            seal, ops = done
            return (_u8(8) + _b(child.encode()) + _u8(0) + _u64(seal) + _u64(len(ops))
                    + b"".join(_u64(op) for op in ops))
        return super()._answer(body)

    def _serve(self, c):
        try:
            super()._serve(c)
        except OSError:
            pass   # the agent went away mid-answer: a kill, or the end of its run

    def roll_back(self):
        """What every start does: the files go back to the last Seal."""
        with self.lock:
            if self.sealed_dir is not None:
                shutil.rmtree(self.work)
                shutil.copytree(self.sealed_dir, self.work, symlinks=True)


def have_programs():
    return all(os.access(os.path.join(BIN, p), os.X_OK) for p in PROGRAMS)


def run(program, *args):
    return subprocess.run([os.path.join(BIN, program), *args],
                          check=True, capture_output=True, text=True).stdout.strip()


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def wait_for(what, ok, seconds=15.0):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if ok():
            return
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {what}")


@unittest.skipUnless(have_programs(), f"build {', '.join(PROGRAMS)} first (ISLAND_BIN={BIN})")
class Converse(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="s4-converse-")
        self.procs = {}
        k = lambda name: os.path.join(self.tmp, name + ".key")  # noqa: E731
        for name in ("root", "appender", "socket", "sender", "daily"):
            run("island-cli", "keygen", k(name))
        os.makedirs(os.path.join(self.tmp, "vessels"))
        vessel_key = os.path.join(self.tmp, "vessels", "0123456789abcdef0123456789abcdef.key")
        run("island-cli", "keygen", vessel_key)
        self.root = run("island-cli", "public", k("root"))
        log = os.path.join(self.tmp, "island.log")
        run("island-cli", "init", "--log", log, "--appender-key", k("appender"), "--root-key", k("root"))
        for holder, scope, rights in ((k("socket"), "access", "access"), (k("sender"), "approvals", "dispatch"),
                                      (k("daily"), "approvals", "approval"), (vessel_key, "flare", "verse")):
            run("island-cli", "grant", "--log", log, "--appender-key", k("appender"), "--root-key", k("root"),
                "--holder", run("island-cli", "public", holder), "--scope", scope, "--rights", rights)
        self.log, self.daily, self.vessel_key = log, k("daily"), vessel_key
        port = free_port()
        self.broker = f"http://127.0.0.1:{port}"
        self.spawn("island-broker", [os.path.join(BIN, "island-broker"), "--log", log, "--appender-key",
                                     k("appender"), "--root", self.root, "--listen", f"127.0.0.1:{port}"])
        wait_for("the broker", lambda: self.connects(port))

        ScriptedModel.requests = []
        model = http.server.ThreadingHTTPServer(("127.0.0.1", 0), ScriptedModel)
        threading.Thread(target=model.serve_forever, daemon=True).start()
        self.addCleanup(model.shutdown)

        self.agent_sock = os.path.join(self.tmp, "agent.sock")
        self.session_port = free_port()
        uid = str(os.getuid())
        self.spawn("flare-server", [os.path.join(BIN, "flare-server"), "--broker", self.broker,
                                    "--root", self.root, "--data", os.path.join(self.tmp, "flare"),
                                    "--listen", f"127.0.0.1:{self.session_port}",
                                    "--agent-socket", self.agent_sock, "--task-reader", uid,
                                    "--reply-sender", uid])
        wait_for("flare-server", lambda: os.path.exists(self.agent_sock))

        base = os.getuid() - os.getuid() % RANGE
        with open(os.path.join(self.tmp, "warden.json"), "w") as out:
            json.dump({"runtime": {"host_uid_base": base},
                       "vessels": [{"namespace": "151a4d00000000000000000000000052", "subject": "flare-home",
                                    "uid_slot": (os.getuid() - base) // RANGE,
                                    "forks": {"slots": FORK_SLOTS}}]}, out)
        self.uid_base = base
        self.island = os.path.join(self.tmp, "island.sock")
        with open(os.path.join(self.tmp, "socket.toml"), "w") as out:
            out.write(f'socket = "{self.island}"\nwarden_config = "{self.tmp}/warden.json"\n'
                      f'state_dir = "{self.tmp}/state"\nbroker = "{self.broker}"\nroot = "{self.root}"\n'
                      f'key = "{k("socket")}"\naccess_every_secs = 1\ntasks = "{self.agent_sock}"\n'
                      f'default = "scripted"\n[provider.scripted]\n'
                      f'base_url = "http://127.0.0.1:{model.server_address[1]}/v1"\n'
                      f'models = ["scripted"]\nbudget = {{ calls = 100, bytes = 10000000 }}\n')
        self.spawn("island-socket", [os.path.join(BIN, "island-socket"), "serve", "--config",
                                     os.path.join(self.tmp, "socket.toml")])
        wait_for("the island socket", lambda: os.path.exists(self.island))

        # The children run as other uids: they reach the island socket,
        # and a copy of the agent, through this directory.
        os.chmod(self.tmp, 0o755)
        self.agent_bin = os.path.join(self.tmp, "flare-agent")
        shutil.copy(os.path.join(BIN, "flare-agent"), self.agent_bin)
        os.chmod(self.agent_bin, 0o755)
        self.work = os.path.join(self.tmp, "work")
        os.makedirs(self.work)
        self.children, self.promotions, self.starts = {}, [], 0
        self.stopping = threading.Event()
        self.warden = ForkingWarden(self.tmp, self.work, world=self)
        self.start_agent()
        self.spawn("island-sender", [os.path.join(BIN, "island-sender"), "--socket", self.warden.sender_path,
                                     "--broker", self.broker, "--root", self.root, "--key", k("sender"),
                                     "--session-socket", self.agent_sock, "--interval-ms", "100",
                                     "--vessel-keys", os.path.join(self.tmp, "vessels")])

    def start_agent(self):
        self.spawn("flare-agent", [self.agent_bin, "--field", self.warden.field_path,
                                   "--island", self.island, "--work", self.work])

    def connects(self, port):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return True
        except OSError:
            return False

    def spawn(self, name, argv):
        err = open(os.path.join(self.tmp, f"{name}.err"), "a")
        self.addCleanup(err.close)
        self.procs[name] = subprocess.Popen(argv, stdout=err, stderr=err)

    def tearDown(self):
        self.stopping.set()
        time.sleep(0.1)
        for p in reversed(list(self.procs.values())):
            if p.poll() is None:
                p.terminate()
                p.wait(10)

    def say(self, text, *extra, after="999999", client="test"):
        argv = [os.path.join(BIN, "flare-tui"), "--server", f"ws://127.0.0.1:{self.session_port}/session",
                "--session", "main", "--client", client, "--root", self.root,
                "--broker", self.broker, "--replica", os.path.join(self.tmp, "replica.log"),
                "--reply-wait", "60", *extra]
        if after is not None:
            argv += ["--after", after]
        out = subprocess.run(argv, input=f"{text}\n", capture_output=True, text=True, timeout=90)
        return out.stdout + out.stderr

    def tail(self):
        return run("island-cli", "tail", "--broker", self.broker, "--root", self.root, "-n", "50")

    def events(self, kind):
        path = os.path.join(self.tmp, "flare", "sessions", "main.jsonl")
        with open(path) as fh:
            return [e for e in map(json.loads, fh) if e["kind"] == kind]

    def replies(self):
        path = os.path.join(self.tmp, "flare", "sessions", "main.jsonl")
        with open(path) as fh:
            events = [json.loads(line) for line in fh]
        return [e["payload"] for e in events if e["kind"] == "agent_text"]

    def agent_log(self):
        with open(os.path.join(self.work, ".flare", "log.jsonl")) as fh:
            return [json.loads(line) for line in fh]

    def test_a_message_becomes_a_task_and_its_reply_comes_back_once(self):
        out = self.say("draft the verse for that")
        self.assertIn("test → agent: draft the verse for that", out, out)
        self.assertIn("agent (to #0): I wrote hello.txt for: draft the verse for that", out, out)
        with open(os.path.join(self.work, "hello.txt")) as fh:
            self.assertEqual(fh.read(), "draft the verse for that")
        replies = self.replies()
        self.assertEqual([(r["task"], r["vessel"]) for r in replies], [(0, "0123456789abcdef0123456789abcdef")])
        # The task was told with who said it; the model was named by the socket.
        first = ScriptedModel.requests[0]
        self.assertEqual(first["model"], "scripted")
        self.assertIn("[Task #0 from test]", first["messages"][1]["content"])
        # The turn ended: the Warden sealed it, the transcript holds the calls.
        wait_for("the turn", lambda: self.warden.last_sealed == 1)
        log = self.agent_log()
        self.assertEqual([e["event"] for e in log], ["start", "task", "turn"])
        # The reply was released, delivered and settled.
        wait_for("the reply settled", lambda: not self.warden.released)

        # A new start goes on after the task: the second message only.
        self.procs["flare-agent"].kill()
        self.procs["flare-agent"].wait()
        self.start_agent()
        out = self.say("and another")
        self.assertIn("agent (to #2): I wrote hello.txt for: and another", out, out)
        self.assertEqual([r["task"] for r in self.replies()], [0, 2])
        self.assertEqual(len({r["key"] for r in self.replies()}), 2)
        starts = [e for e in self.agent_log() if e["event"] == "start"]
        self.assertEqual(starts[-1]["cursor"], 0)


    def test_a_verse_waits_for_approval_and_is_written_once_signed_as_the_vessel(self):
        out = self.say("please propose a verse about this")
        self.assertIn("agent (to #0): I proposed a verse; it waits for you.", out, out)
        wait_for("the action waiting", lambda: self.events("action_waiting"))
        waiting = self.events("action_waiting")
        self.assertEqual(len(waiting), 1)
        self.assertEqual(waiting[0]["payload"]["shown"],
                         'a verse on flare: [decision] "Verses from the agent wait for approval"')
        seq = waiting[0]["seq"]
        # Nothing is written before it is approved.
        self.assertFalse([line for line in self.tail().splitlines() if line.split()[2:3] == ["verse"]])

        # Approved from the terminal, with a key that may approve.
        out = self.say(f"/approve {seq}", "--key", self.daily, after=None)
        self.assertIn(f"approve request {waiting[0]['payload']['request_hash'][:8]}", out, out)
        wait_for("the action settled", lambda: self.events("action_settled"), seconds=20)
        settled = self.events("action_settled")[0]["payload"]
        self.assertEqual(settled["settled"], "sent", settled)
        # In the log once, on flare, by the Vessel's key.
        vessel_pub = run("island-cli", "public", self.vessel_key)[:8]
        tail = self.tail()
        verses = [line for line in tail.splitlines()
                  if line.split()[2:5] == ["verse", "flare", vessel_pub]]
        self.assertEqual(len(verses), 1, tail)
        self.assertIn("Verses from the agent wait for approval", verses[0])
        self.assertEqual(int(verses[0].split()[0]), settled["entry"])
        # Approving again lets nothing more out: the action is settled.
        time.sleep(1)
        self.assertEqual(len(self.events("action_settled")), 1)


    # ── forks (step 6) ──────────────────────────────────────────────

    def fork(self, prime):
        """Taso's Fork: a child in the next fork slot, on the prime's newest Seal."""
        if len(self.children) == len(FORK_SLOTS):
            return f"no fork slot left: all {len(FORK_SLOTS)} of \"flare-home\"'s are taken"
        slot = FORK_SLOTS[len(self.children)]
        name = f"flare-home-fork-{slot}"
        uid = self.uid_base + slot * RANGE
        home = os.path.join(self.tmp, f"child-{slot}")
        work = os.path.join(home, "work")
        os.makedirs(home)
        shutil.copytree(prime.sealed_dir, work, symlinks=True)
        for top, dirs, files in os.walk(work):
            for entry in [top] + [os.path.join(top, name_) for name_ in files]:
                os.chown(entry, uid, uid)
        child = ForkingWarden(home, work, hold=True, last_sealed=prime.last_sealed)
        os.chmod(home, 0o755)
        os.chmod(child.field_path, 0o666)
        self.children[name] = child
        err = open(os.path.join(self.tmp, f"{name}.err"), "a")
        self.addCleanup(err.close)
        self.procs[name] = subprocess.Popen(
            [self.agent_bin, "--field", child.field_path, "--island", self.island, "--work", work],
            stdout=err, stderr=err, user=uid, group=uid, extra_groups=[])
        return name, 100 + prime.last_sealed

    def promote(self, prime, name):
        """Taso's Promote: the child's newest Seal becomes the prime's files,
        its held actions the prime's released ones, unknown."""
        child = self.children.get(name)
        if child is None or child.sealed_dir is None:
            return f"no fork {name!r} with a Seal"
        with prime.lock:
            ops = []
            for provider, request, key, consequence in child.held:
                op, prime.next_op = prime.next_op, prime.next_op + 1
                prime.released[op] = (provider, request, key, consequence, True)
                if consequence == f.IRREVERSIBLE:
                    prime.authority = False
                ops.append(op)
            kept = os.path.join(prime.seals, f"promoted-{child.last_sealed}")
            shutil.copytree(child.sealed_dir, kept, symlinks=True)
            for top, dirs, files in os.walk(kept):
                for entry in [top] + [os.path.join(top, name_) for name_ in files]:
                    os.chown(entry, os.getuid(), os.getgid())
            prime.sealed_dir = kept
            prime.last_sealed = child.last_sealed
        self.promotions.append(name)
        return 100 + child.last_sealed, ops

    def keep_the_prime_running(self):
        """The Warden's `restart: always`: when the agent's run ends, its
        files go back to the last Seal and it is started again."""
        def watch():
            while not self.stopping.is_set():
                agent = self.procs["flare-agent"]
                if agent.poll() is None:
                    time.sleep(0.05)
                    continue
                self.warden.roll_back()
                self.starts += 1
                self.start_agent()
        threading.Thread(target=watch, daemon=True).start()

    def whoami(self):
        conn = http.client.HTTPConnection("island")
        conn.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        conn.sock.connect(self.island)
        try:
            conn.request("GET", "/whoami")
            return json.loads(conn.getresponse().read())
        finally:
            conn.close()

    @unittest.skipUnless(os.getuid() == 0, "a fork child needs a uid of its own: run as root")
    def test_a_task_is_forked_two_ways_and_the_promoted_child_is_the_agents_files(self):
        self.keep_the_prime_running()
        out = self.say("please fork this into two drafts")
        # The promoted child's held answer is the reply to the task.
        self.assertIn("agent (to #0): Child 2 drafted draft-2.md.", out, out)
        self.assertEqual(self.promotions, ["flare-home-fork-91"])
        # Each child said it started and what it made; the session shows it.
        notes = [e["payload"] for e in self.events("fork_note")]
        self.assertEqual(sorted((n["child"], n["done"]) for n in notes),
                         [("flare-home-fork-90", False), ("flare-home-fork-90", True),
                          ("flare-home-fork-91", False), ("flare-home-fork-91", True)])
        done = {n["child"]: n["text"] for n in notes if n["done"]}
        self.assertTrue(done["flare-home-fork-90"].startswith("Child 1 drafted draft-1.md."), done)
        self.assertIn("0 other message(s), 1 verse(s)", done["flare-home-fork-91"])
        # Both children were forked from the one Seal the fork call made,
        # each started on its files, and neither is the other's.
        children = [self.children[f"flare-home-fork-{slot}"] for slot in FORK_SLOTS]
        self.assertEqual([c.last_sealed for c in children], [2, 2])
        self.assertTrue(os.path.exists(os.path.join(children[0].work, "draft-1.md")))
        self.assertFalse(os.path.exists(os.path.join(children[0].work, "draft-2.md")))
        # The prime was started again, on the promoted child's files.
        wait_for("the prime's next start", lambda: self.starts == 1)
        wait_for("the prime on the child's files",
                 lambda: [e["event"] for e in self.agent_log()][-1] == "promoted")
        with open(os.path.join(self.work, "draft-2.md")) as fh:
            self.assertEqual(fh.read(), "Draft it as a list, in draft-2.md, with a verse")
        self.assertFalse(os.path.exists(os.path.join(self.work, "draft-1.md")))
        self.assertFalse(os.path.exists(os.path.join(self.work, ".flare", "fork-tasks.json")))
        log = self.agent_log()
        self.assertEqual([e["event"] for e in log],
                         ["start", "task", "turn", "start", "child", "turn", "start", "promoted"], log)
        self.assertEqual(log[2].get("fork_set"), "0-1")
        self.assertEqual(log[7]["child"], "flare-home-fork-91")
        # Two fork slots, both used, for good.
        self.assertEqual(self.whoami()["forks_left"], 0)

        # The child's verse was adopted with its reply: it waits for
        # Jarmo, and until he answers the agent can release nothing new.
        wait_for("the adopted verse waiting", lambda: self.events("action_waiting"))
        waiting = self.events("action_waiting")
        self.assertEqual(waiting[0]["payload"]["shown"],
                         'a verse on flare: [decision] "Fork child 2 drafted it as a list"')
        said = {}
        asking = threading.Thread(target=lambda: said.update(out=self.say("and another")))
        asking.start()
        wait_for("the next task worked on",
                 lambda: any(e["event"] == "task" and e["task"] != 0 for e in self.agent_log()))
        time.sleep(2)
        self.assertEqual([r["task"] for r in self.replies()], [0], "its reply waits for the verse's answer")
        # From another terminal: a terminal knows its commands by its name.
        out = self.say(f"/approve {waiting[0]['seq']}", "--key", self.daily, after=None, client="laptop")
        self.assertIn("approve request", out, out)
        asking.join(90)
        self.assertIn("I wrote hello.txt for: and another", said.get("out", ""), said)
        self.assertEqual(len(self.replies()), 2)
        wait_for("the verse settled", lambda: self.events("action_settled"), seconds=20)
        self.assertEqual(self.events("action_settled")[0]["payload"]["settled"], "sent")
        vessel_pub = run("island-cli", "public", self.vessel_key)[:8]
        verses = [line for line in self.tail().splitlines()
                  if line.split()[2:5] == ["verse", "flare", vessel_pub]]
        self.assertEqual(len(verses), 1, self.tail())
        self.assertIn("Fork child 2 drafted it as a list", verses[0])
        # With its next task the model was told that it is the prime again.
        told = [m["content"] for body in ScriptedModel.requests for m in body["messages"]
                if m["role"] == "user" and "and another" in m["content"]]
        self.assertTrue(told and told[-1].startswith(
            "[Flare: on task #0 you forked 2 child(ren) and promoted flare-home-fork-91 (child 2)."), told)
        # A third fork has no slot: said to the model before any Seal.
        out = self.say("fork it again")
        self.assertIn("I could not fork: not forked: 2 task(s), but 0 of this Vessel's 2 fork slots are left.",
                      out, out)


if __name__ == "__main__":
    unittest.main()
