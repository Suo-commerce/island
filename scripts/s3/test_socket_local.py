"""island-socket end to end, without Taso: a real Island log and broker, a
stub model (stub-model.py), and the socket served by the real program.
The caller is this process: the Warden config puts its uid in flare-home's
slot.  Checks what the agent gets, and the `access` entries the log gets.

Needs the built programs: ISLAND_BIN names their directory (default
target/debug); skipped if they are missing.  Run: `just s3-test`, or
python3 -m unittest scripts/s3/test_socket_local.py after
`cargo build -p island-cli -p island-broker -p island-socket`.
"""

import http.client
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.environ.get("ISLAND_BIN", os.path.join(HERE, "..", "..", "target", "debug"))
PROGRAMS = ("island-cli", "island-broker", "island-socket")
FLARE_HOME = "914c503c980b8f7f1750c664b5743d2c"  # flare-home's Vessel id on the server
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


def wait_for(what, ok, seconds=10.0):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if ok():
            return
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {what}")


class UnixHTTP(http.client.HTTPConnection):
    """HTTP over a Unix socket: the agent's side of the island socket."""

    def __init__(self, path):
        super().__init__("island")
        self.path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(self.path)


@unittest.skipUnless(have_programs(), f"build {', '.join(PROGRAMS)} first (ISLAND_BIN={BIN})")
class Socket(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="s3-socket-")
        self.procs = []
        k = lambda name: os.path.join(self.tmp, name + ".key")  # noqa: E731
        for name in ("root", "appender", "socket"):
            run("island-cli", "keygen", k(name))
        self.root = run("island-cli", "public", k("root"))
        log = os.path.join(self.tmp, "island.log")
        run("island-cli", "init", "--log", log, "--appender-key", k("appender"), "--root-key", k("root"))
        run("island-cli", "grant", "--log", log, "--appender-key", k("appender"), "--root-key", k("root"),
            "--holder", run("island-cli", "public", k("socket")), "--scope", "access", "--rights", "access")
        port = free_port()
        self.broker = f"http://127.0.0.1:{port}"
        self.spawn([os.path.join(BIN, "island-broker"), "--log", log, "--appender-key", k("appender"),
                    "--root", self.root, "--listen", f"127.0.0.1:{port}"])
        wait_for("the broker", lambda: self.connects(port))
        model_port = free_port()
        self.requests = os.path.join(self.tmp, "requests.jsonl")
        self.spawn([sys.executable, os.path.join(HERE, "stub-model.py"), "--listen", f"127.0.0.1:{model_port}",
                    "--bytes", "100000", "--pieces", "200", "--delay", "0", "--requests", self.requests])
        wait_for("the stub model", lambda: self.connects(model_port))
        uid = os.getuid()
        base = uid - uid % RANGE
        with open(os.path.join(self.tmp, "warden.json"), "w") as out:
            json.dump({"runtime": {"host_uid_base": base},
                       "vessels": [{"namespace": "151a4d00000000000000000000000052", "subject": "flare-home",
                                    "uid_slot": (uid - base) // RANGE, "forks": {"slots": [90, 91]}}]}, out)
        with open(os.path.join(self.tmp, "stub.key"), "w") as out:
            out.write("sk-stub\n")
        self.sock = os.path.join(self.tmp, "island.sock")
        self.config = os.path.join(self.tmp, "config.toml")
        self.write_config(every=1)
        self.key = k("socket")

    def write_config(self, every):
        with open(self.config, "w") as out:
            out.write(f'socket = "{self.sock}"\nwarden_config = "{self.tmp}/warden.json"\n'
                      f'state_dir = "{self.tmp}/state"\nbroker = "{self.broker}"\nroot = "{self.root}"\n'
                      f'key = "{self.tmp}/socket.key"\naccess_every_secs = {every}\ndefault = "stub"\n'
                      f'[provider.stub]\nbase_url = "http://127.0.0.1:{self.model_port()}/v1"\n'
                      f'models = ["stub-small"]\nkey_file = "{self.tmp}/stub.key"\n'
                      f'budget = {{ calls = 100, bytes = 100000000 }}\n')

    def model_port(self):
        # The stub's port, from its command line.
        return int(self.procs[1].args[3].rsplit(":", 1)[1])

    def connects(self, port):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            return True
        except OSError:
            return False

    def spawn(self, argv):
        err = open(os.path.join(self.tmp, f"{os.path.basename(argv[1] if argv[0] == sys.executable else argv[0])}.err"), "a")
        self.addCleanup(err.close)
        p = subprocess.Popen(argv, stderr=err)
        self.procs.append(p)
        return p

    def tearDown(self):
        for p in reversed(self.procs):
            if p.poll() is None:
                p.terminate()
                p.wait(10)

    def serve(self):
        p = self.spawn([os.path.join(BIN, "island-socket"), "serve", "--config", self.config])
        wait_for("the island socket", lambda: os.path.exists(self.sock))
        return p

    def ask(self, method, path, body=None):
        c = UnixHTTP(self.sock)
        c.request(method, path, body=body, headers={"content-type": "application/json"})
        r = c.getresponse()
        data = r.read()
        c.close()
        return r.status, data

    def tail(self):
        return run("island-cli", "tail", "--broker", self.broker, "--root", self.root, "-n", "20")

    def accesses(self):
        return [line for line in self.tail().splitlines() if line.split()[2:3] == ["access"]]

    def digest(self, request, response):
        req, res = os.path.join(self.tmp, "req"), os.path.join(self.tmp, "res")
        with open(req, "wb") as out:
            out.write(request)
        with open(res, "wb") as out:
            out.write(response)
        call = run("island-socket", "call-digest", "--request", req, "--response", res)
        return call, run("island-socket", "batch-digest", call)

    def test_whoami_and_a_streamed_call_reach_the_log_as_one_access_entry(self):
        self.serve()
        status, body = self.ask("GET", "/whoami")
        self.assertEqual(status, 200, body)
        me = json.loads(body)
        self.assertEqual((me["vessel"], me["role"], me["forks_left"]), (FLARE_HOME, "prime", 2))

        request = json.dumps({"model": "stub-small", "stream": True,
                              "messages": [{"role": "user", "content": "hello"}]}).encode()
        status, answer = self.ask("POST", "/v1/chat/completions", request)
        self.assertEqual(status, 200, answer[:200])
        self.assertTrue(answer.endswith(b"data: [DONE]\n\n"))
        self.assertGreater(answer.count(b"data: "), 100, "streamed in pieces")
        with open(self.requests) as fh:
            seen = [json.loads(line) for line in fh]
        self.assertEqual(seen[0]["body"].encode(), request, "the agent's bytes, as they are")
        self.assertEqual(seen[0]["auth"], "Bearer sk-stub", "the key is the socket's")

        wait_for("an access entry", lambda: len(self.accesses()) == 1)
        _, batch = self.digest(request, answer)
        entry = self.accesses()[0]
        self.assertIn(f"vessel {FLARE_HOME} Inference via \"stub\", 1 call(s), "
                      f"{len(request)} bytes in, {len(answer)} out, digest {batch[:8]}", entry)
        self.assertIn(f" {run('island-cli', 'public', self.key)[:8]} ", entry, "written by the socket's key")

    def test_calls_batch_and_the_last_go_out_at_sigterm(self):
        self.write_config(every=3600)
        p = self.serve()
        calls = []
        for i in range(3):
            request = json.dumps({"model": "stub-small", "messages": [{"role": "user", "content": str(i)}]}).encode()
            status, answer = self.ask("POST", "/v1/chat/completions", request)
            self.assertEqual(status, 200)
            calls.append(self.digest(request, answer)[0])
        time.sleep(0.3)
        self.assertEqual(self.accesses(), [], "nothing before the hour")
        p.send_signal(signal.SIGTERM)
        self.assertEqual(p.wait(10), 0)
        entries = self.accesses()
        self.assertEqual(len(entries), 1, entries)
        self.assertIn("3 call(s)", entries[0])
        self.assertIn(f"digest {run('island-socket', 'batch-digest', *calls)[:8]}", entries[0])

    def test_a_budget_survives_a_restart(self):
        self.write_config(every=1)
        with open(self.config) as fh:
            text = fh.read().replace("calls = 100", "calls = 2")
        with open(self.config, "w") as out:
            out.write(text)
        p = self.serve()
        call = json.dumps({"model": "stub-small", "messages": []}).encode()
        for _ in range(2):
            self.assertEqual(self.ask("POST", "/v1/chat/completions", call)[0], 200)
        p.send_signal(signal.SIGTERM)
        p.wait(10)
        os.remove(self.sock) if os.path.exists(self.sock) else None
        self.serve()
        status, body = self.ask("POST", "/v1/chat/completions", call)
        self.assertEqual(status, 429, body)


if __name__ == "__main__":
    unittest.main()
