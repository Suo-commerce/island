"""Step 4 end to end, without Taso: a message said in the session reaches
flare-agent as a task, and its reply comes back to the session once.

The real programs: island-broker on a real log, flare-server with its
agent socket, island-socket with a scripted model, flare-agent, and
island-sender; a stand-in Warden (scripts/s2/test_s2_flow.py) serves the
Field and the sender socket, and flare-tui says the message. The caller of
every socket is this process: the Warden config puts its uid in
flare-home's slot, and flare-server takes tasks and replies from it.

Needs the built programs: ISLAND_BIN names their directory (default
target/debug); skipped if they are missing. Run: `just agent-test`.
"""

import http.server
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "s2"))
from test_s2_flow import StandIn  # noqa: E402

BIN = os.environ.get("ISLAND_BIN", os.path.join(HERE, "..", "..", "target", "debug"))
PROGRAMS = ("island-cli", "island-broker", "island-socket", "island-sender", "flare-server",
            "flare-agent", "flare-tui")
RANGE = 65536


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


class ScriptedModel(http.server.BaseHTTPRequestHandler):
    """A model that writes a file with a tool call, then says it did; asked
    for a verse, it proposes one."""

    requests = []

    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        ScriptedModel.requests.append(body)
        last_user = [m for m in body["messages"] if m["role"] == "user"][-1]["content"]
        tools_done = body["messages"][-1]["role"] == "tool"
        wants_verse = "propose a verse" in last_user.splitlines()[-1]
        if tools_done and wants_verse:
            chunks = [{"choices": [{"delta": {"content": "\n\nI proposed a verse; it waits for you."}}]},
                      {"choices": [{"delta": {}, "finish_reason": "stop"}]}]
        elif tools_done:
            chunks = [{"choices": [{"delta": {"content": f"I wrote hello.txt for: {last_user.splitlines()[-1]}"}}]},
                      {"choices": [{"delta": {}, "finish_reason": "stop"}]}]
        elif wants_verse:
            args = json.dumps({"evidence": "decision", "text": "Verses from the agent wait for approval"})
            chunks = [{"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call-v", "function": {"name": "verse", "arguments": args}}]}}]},
                {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}]
        else:
            args = json.dumps({"path": "hello.txt", "content": last_user.splitlines()[-1]})
            chunks = [{"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call-1", "function": {"name": "write", "arguments": args}}]}}]},
                {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}]
        payload = "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"
        data = payload.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


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
                                    "uid_slot": (os.getuid() - base) // RANGE, "forks": {"slots": [90]}}]}, out)
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

        self.warden = StandIn(self.tmp)
        self.work = os.path.join(self.tmp, "work")
        os.makedirs(self.work)
        self.start_agent()
        self.spawn("island-sender", [os.path.join(BIN, "island-sender"), "--socket", self.warden.sender_path,
                                     "--broker", self.broker, "--root", self.root, "--key", k("sender"),
                                     "--session-socket", self.agent_sock, "--interval-ms", "100",
                                     "--vessel-keys", os.path.join(self.tmp, "vessels")])

    def start_agent(self):
        self.spawn("flare-agent", [os.path.join(BIN, "flare-agent"), "--field", self.warden.field_path,
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
        for p in reversed(list(self.procs.values())):
            if p.poll() is None:
                p.terminate()
                p.wait(10)

    def say(self, text, *extra, after="999999"):
        argv = [os.path.join(BIN, "flare-tui"), "--server", f"ws://127.0.0.1:{self.session_port}/session",
                "--session", "main", "--client", "test", "--root", self.root,
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


if __name__ == "__main__":
    unittest.main()
