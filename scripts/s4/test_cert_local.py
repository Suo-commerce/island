#!/usr/bin/env python3
"""The face's certificate, on this machine (`just cert-test`).

Two halves, both against the real scripts (scripts/node/cert.sh,
scripts/node/desec.py):

1. The deSEC side against a stand-in for deSEC's API that keeps its
   rules for tokens and their policies: the narrow token is minted,
   written, proved; a token that is too wide is found out.

2. Getting and renewing the certificate against Pebble, Let's Encrypt's
   test CA, with the pinned lego: `cert.sh issue live` gets one and puts
   it in place, a second run leaves it alone, a forced renewal replaces
   it, and a running flare-server serves the new one without a restart.
   Pebble and lego are fetched once (hash checked) into
   target/cert-test; without the network this half is skipped.

What this cannot reach is deSEC itself and Let's Encrypt: `cert.sh try`
on the server is their test.
"""

import hashlib
import http.server
import json
import os
import re
import shutil
import socket
import ssl
import stat
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.join(HERE, "..", "..")
BIN = os.path.join(REPO, "target", "debug")
CERT_SH = os.path.join(REPO, "scripts", "node", "cert.sh")
DESEC_PY = os.path.join(REPO, "scripts", "node", "desec.py")
CACHE = os.path.join(REPO, "target", "cert-test")
NAME = "island.example.net"

FETCH = {
    "lego": ("https://github.com/go-acme/lego/releases/download/v5.6.0/lego_v5.6.0_linux_amd64.tar.gz",
             "f02fc11b6f56b97e387fa7caa0268f86f5a336953b22458e6430e4651fea439a", "lego"),
    "pebble": ("https://github.com/letsencrypt/pebble/releases/download/v2.9.0/pebble-linux-amd64.tar.gz",
               "ca8b055fb6caf44e48f8bdaf351a7c8a643d63efc88f40d70cc1444bce298c04",
               "pebble-linux-amd64/linux/amd64/pebble"),
    "pebble-challtestsrv": ("https://github.com/letsencrypt/pebble/releases/download/v2.9.0/pebble-challtestsrv-linux-amd64.tar.gz",
                            "5f59142f26e80378bc73fabd85b4de15c362595b211ccb4e85f6584421d19913",
                            "pebble-challtestsrv-linux-amd64/linux/amd64/pebble-challtestsrv"),
}


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(what, ready, seconds=30):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            if ready():
                return
        except OSError:
            pass
        time.sleep(0.1)
    raise AssertionError(f"{what} did not come up in {seconds}s")


def connects(port):
    with socket.create_connection(("127.0.0.1", port), timeout=1):
        return True


# ── a stand-in for deSEC's API ───────────────────────────────────────────

class Desec:
    """deSEC as far as desec.py uses it, with its token rules
    (desec.readthedocs.io, "Manage Tokens")."""

    def __init__(self, wide=False, published=None):
        self.wide = wide            # a deSEC that ignores policies: to see the proof fail
        self.published = published  # called with (fqdn, records) when a TXT record changes: its nameservers
        self.tokens = {}            # secret -> token
        self.domains = {}           # name -> {(subname, type): {"ttl", "records"}}
        self.writes = []            # (secret's name, method, path, status)
        self.count = 0

    def token(self, name, manage=False, create=False):
        self.count += 1
        secret = f"secret-{self.count}-{name}"
        self.tokens[secret] = {"id": f"id-{self.count}", "name": name, "perm_manage_tokens": manage,
                               "perm_create_domain": create, "perm_delete_domain": False, "policies": []}
        return secret

    def may_write(self, token, domain, subname, kind):
        if self.wide or not token["policies"]:
            return True
        best, allowed = -1, False
        for policy in token["policies"]:
            score = 0
            for have, want, weight in ((policy["domain"], domain, 4), (policy["subname"], subname, 2), (policy["type"], kind, 1)):
                if have is None:
                    continue
                if have != want:
                    score = None
                    break
                score += weight
            if score is not None and score > best:
                best, allowed = score, policy["perm_write"]
        return allowed

    def handle(self, secret, method, path, body):
        token = self.tokens.get(secret)
        if token is None:
            return 401, {"detail": "Invalid token."}
        restricted = bool(token["policies"]) and not self.wide
        parts = [p for p in path.split("/") if p]
        if parts[:2] == ["auth", "tokens"]:
            if not token["perm_manage_tokens"]:
                return 403, {"detail": "Insufficient token permissions."}
            if len(parts) == 2 and method == "GET":
                return 200, [{k: v for k, v in t.items() if k != "policies"} for t in self.tokens.values()]
            if len(parts) == 2 and method == "POST":
                new = self.token(body["name"], body.get("perm_manage_tokens", False), body.get("perm_create_domain", False))
                return 201, {**{k: v for k, v in self.tokens[new].items() if k != "policies"}, "token": new}
            target = next((s for s, t in self.tokens.items() if t["id"] == parts[2]), None)
            if target is None:
                return 404, {"detail": "Not found."}
            if len(parts) == 3 and method == "DELETE":
                del self.tokens[target]
                return 204, None
            if parts[3:] == ["policies", "rrsets"] and method == "POST":
                policies = self.tokens[target]["policies"]
                default = all(body.get(k) is None for k in ("domain", "subname", "type"))
                if not default and not policies:
                    return 400, {"detail": "Policy precedence: The first policy must be the default policy."}
                policies.append({"domain": body.get("domain"), "subname": body.get("subname"),
                                 "type": body.get("type"), "perm_write": body.get("perm_write", False)})
                return 201, policies[-1]
        if parts[:1] == ["domains"]:
            if len(parts) == 1 and method == "POST":
                if not token["perm_create_domain"]:
                    return 403, {"detail": "Insufficient token permissions."}
                self.domains[body["name"]] = {}
                return 201, {"name": body["name"], "minimum_ttl": 3600}
            zone = self.domains.get(parts[1])
            if zone is None:
                return 404, {"detail": "Not found."}
            if len(parts) == 2 and method == "GET":
                return 200, {"name": parts[1], "minimum_ttl": 3600}
            if parts[2:] == ["status"]:
                return 200, {"nameserver_status": "not_delegated", "nameservers": [], "checked": None}
            if parts[2] == "rrsets" and len(parts) == 3 and method == "POST":
                key = (body.get("subname", ""), body["type"])
                if not self.may_write(token, parts[1], *key):
                    return 403, {"detail": "You do not have permission to perform this action."}
                if key in zone:
                    return 400, {"non_field_errors": ["Another RRset with the same subdomain and type exists for this domain."]}
                zone[key] = {"ttl": body["ttl"], "records": body["records"]}
                self.publish(parts[1], key)
                return 201, {"subname": key[0], "type": key[1], **zone[key]}
            if parts[2] == "rrsets" and len(parts) == 5:
                key = ("" if parts[3] == "@" else parts[3], parts[4])
                if method == "GET":
                    if key not in zone:
                        return 404, {"detail": "Not found."}
                    return 200, {"subname": key[0], "type": key[1], **zone[key]}
                if not self.may_write(token, parts[1], *key):
                    return 403, {"detail": "You do not have permission to perform this action."}
                if method == "DELETE" or (method == "PATCH" and body.get("records") == []):
                    zone.pop(key, None)
                    self.publish(parts[1], key)
                    return 204, None
                if method == "PATCH":
                    if key not in zone:
                        return 404, {"detail": "Not found."}
                    zone[key].update({k: body[k] for k in ("ttl", "records") if k in body})
                    self.publish(parts[1], key)
                    return 200, {"subname": key[0], "type": key[1], **zone[key]}
        if restricted:
            return 403, {"detail": "Insufficient token permissions."}
        return 404, {"detail": "Not found."}

    def publish(self, domain, key):
        if self.published and key[1] == "TXT":
            records = self.domains[domain].get(key, {}).get("records", [])
            self.published(f"{key[0]}.{domain}.", [json.loads(r) for r in records])

    def serve(self):
        desec = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def answer(self):
                length = int(self.headers.get("Content-Length") or 0)
                body = json.loads(self.rfile.read(length)) if length else None
                secret = (self.headers.get("Authorization") or "").removeprefix("Token ")
                status, said = desec.handle(secret, self.command, self.path.removeprefix("/api/v1"), body)
                if self.command != "GET":
                    desec.writes.append((desec.tokens.get(secret, {}).get("name"), self.command, self.path, status))
                raw = b"" if said is None else json.dumps(said).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            do_GET = do_POST = do_PATCH = do_DELETE = answer

            def log_message(self, *_):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        return server


class TheDesecSide(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="cert-desec-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.env = os.path.join(self.tmp, "cert.env")

    def desec(self, **how):
        desec = Desec(**how)
        server = desec.serve()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.api = f"http://127.0.0.1:{server.server_address[1]}/api/v1"
        return desec

    def run_desec(self, *args, setup=None):
        env = {**os.environ, "DESEC_API": self.api}
        if setup:
            env["DESEC_SETUP_TOKEN"] = setup
        return subprocess.run(["python3", DESEC_PY, *args, "--name", NAME, "--env", self.env],
                              env=env, capture_output=True, text=True, timeout=60)

    def kept(self):
        with open(self.env) as file:
            return dict(line.strip().split("=", 1) for line in file if "=" in line and not line.startswith("#"))

    def test_mint_makes_the_zone_the_address_and_a_token_that_writes_one_record(self):
        desec = self.desec()
        setup = desec.token("island-setup", manage=True, create=True)
        with open(self.env, "w") as file:
            file.write(f"CERT_NAME={NAME}\n")
        done = self.run_desec("mint", "--address", "10.77.0.1", setup=setup)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertNotIn("FAIL", done.stdout)
        self.assertEqual(desec.domains[NAME][("", "A")]["records"], ["10.77.0.1"])
        kept = self.kept()
        self.assertEqual(kept["CERT_NAME"], NAME)
        self.assertEqual(stat.S_IMODE(os.stat(self.env).st_mode), 0o600)
        token = desec.tokens[kept["DESEC_TOKEN"]]
        self.assertEqual((token["name"], token["perm_manage_tokens"], token["perm_create_domain"]), ("island-cert", False, False))
        self.assertEqual(token["policies"], [
            {"domain": None, "subname": None, "type": None, "perm_write": False},
            {"domain": NAME, "subname": "_acme-challenge", "type": "TXT", "perm_write": True}])
        # The pasted token is gone, and nothing it proved with is left behind.
        self.assertNotIn(setup, desec.tokens)
        self.assertNotIn(setup, done.stdout + done.stderr)
        self.assertNotIn(kept["DESEC_TOKEN"], done.stdout + done.stderr)
        self.assertEqual(set(desec.domains[NAME]), {("", "A")})
        for server in ("ns1.desec.io", "ns2.desec.org"):
            self.assertIn(f"type NS    name island    value {server}", done.stdout)

        # The kept token, later: still narrow; nothing written to find that out.
        before = len(desec.writes)
        again = self.run_desec("verify")
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertTrue(all(status == 403 for _, _, _, status in desec.writes[before:]), desec.writes[before:])

        # A second mint replaces the kept token; the old one stops working.
        setup = desec.token("island-setup", manage=True)
        done = self.run_desec("mint", "--address", "10.77.0.1", setup=setup)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertNotIn(kept["DESEC_TOKEN"], desec.tokens)
        self.assertIn(self.kept()["DESEC_TOKEN"], desec.tokens)
        self.assertIn("A 10.77.0.1 is there", done.stdout)

    def test_a_domain_made_by_hand_with_another_address_is_put_right(self):
        desec = self.desec()
        desec.domains[NAME] = {("", "A"): {"ttl": 3600, "records": ["192.0.2.7"]}}
        setup = desec.token("my-token", manage=True)
        done = self.run_desec("mint", "--address", "10.77.0.1", setup=setup)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual(desec.domains[NAME][("", "A")]["records"], ["10.77.0.1"])
        # Not named island-setup: left alone, and said so.
        self.assertIn(setup, desec.tokens)
        self.assertIn("delete it in deSEC's web interface", done.stdout)

    def test_what_is_wrong_is_said_and_nothing_is_kept(self):
        desec = self.desec()
        done = self.run_desec("mint", setup="not-a-token")
        self.assertEqual(done.returncode, 2)
        self.assertIn("does not know that token", done.stderr)
        done = self.run_desec("mint", setup=desec.token("plain"))
        self.assertEqual(done.returncode, 2)
        self.assertIn("may not manage tokens", done.stderr)
        done = self.run_desec("mint", setup=desec.token("no-domains", manage=True))
        self.assertEqual(done.returncode, 2)
        self.assertIn(f"Add the domain {NAME}", done.stderr)
        self.assertFalse(os.path.exists(self.env))
        self.assertEqual([t["name"] for t in desec.tokens.values() if t["name"] == "island-cert"], [])

    def test_a_token_that_can_write_more_is_found_out(self):
        desec = self.desec(wide=True)
        setup = desec.token("island-setup", manage=True, create=True)
        done = self.run_desec("mint", "--address", "10.77.0.1", setup=setup)
        self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
        self.assertIn("FAIL\ta write to another record is refused (201)", done.stdout)
        self.assertIn("FAIL\ta write to the name's address is refused (200)", done.stdout)
        # What the proof wrote, it took away again.
        self.assertEqual(set(desec.domains[NAME]), {("", "A")})


# ── getting and renewing, against Pebble ─────────────────────────────────

def fetched(name):
    """The pinned program, from the cache or the network; None without either."""
    url, digest, member = FETCH[name]
    path = os.path.join(CACHE, name)
    if os.path.isfile(path):
        return path
    os.makedirs(CACHE, exist_ok=True)
    archive = path + ".tar.gz"
    done = subprocess.run(["curl", "-fsSL", "--max-time", "120", "-o", archive, url], capture_output=True)
    if done.returncode != 0:
        return None
    with open(archive, "rb") as file:
        if hashlib.sha256(file.read()).hexdigest() != digest:
            raise AssertionError(f"{url} is not the pinned file")
    with tarfile.open(archive) as tar, open(path, "wb") as out:
        shutil.copyfileobj(tar.extractfile(member), out)
    os.chmod(path, 0o755)
    os.unlink(archive)
    return path


def openssl(*args):
    return subprocess.run(["openssl", *args], check=True, capture_output=True, text=True).stdout


class GettingAndRenewing(unittest.TestCase):
    def setUp(self):
        self.lego, self.pebble, self.challtestsrv = (fetched(n) for n in ("lego", "pebble", "pebble-challtestsrv"))
        if not all((self.lego, self.pebble, self.challtestsrv)):
            self.skipTest("lego and Pebble could not be fetched (no network)")
        if not os.path.isfile(os.path.join(BIN, "flare-server")):
            self.skipTest("flare-server is not built (just cert-test builds it)")
        self.tmp = tempfile.mkdtemp(prefix="cert-pebble-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.procs = []
        self.addCleanup(self.stop)
        t = lambda name: os.path.join(self.tmp, name)  # noqa: E731

        # Pebble's own https needs a certificate lego trusts.
        openssl("req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-days", "2",
                "-subj", "/CN=pebble's own", "-keyout", t("ca.key"), "-out", t("ca.pem"),
                "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign")
        openssl("req", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-subj", "/CN=localhost",
                "-keyout", t("pebble.key"), "-out", t("pebble.csr"))
        with open(t("san"), "w") as file:
            file.write("subjectAltName=DNS:localhost,IP:127.0.0.1\nauthorityKeyIdentifier=keyid\nsubjectKeyIdentifier=hash\n")
        openssl("x509", "-req", "-in", t("pebble.csr"), "-CA", t("ca.pem"), "-CAkey", t("ca.key"), "-CAcreateserial",
                "-days", "2", "-extfile", t("san"), "-out", t("pebble.pem"))

        self.dns, self.dns_admin = free_port(), free_port()
        self.acme, self.acme_admin = free_port(), free_port()
        self.spawn([self.challtestsrv, "-defaultIPv6", "", "-defaultIPv4", "127.0.0.1", "-dns01", f"127.0.0.1:{self.dns}",
                    "-management", f"127.0.0.1:{self.dns_admin}", "-http01", "", "-https01", "", "-tlsalpn01", "", "-doh", ""])
        with open(t("pebble.json"), "w") as file:
            json.dump({"pebble": {"listenAddress": f"127.0.0.1:{self.acme}",
                                  "managementListenAddress": f"127.0.0.1:{self.acme_admin}",
                                  "certificate": t("pebble.pem"), "privateKey": t("pebble.key"),
                                  "httpPort": 5002, "tlsPort": 5001, "ocspResponderURL": "",
                                  "externalAccountBindingRequired": False}}, file)
        self.spawn([self.pebble, "-config", t("pebble.json"), "-dnsserver", f"127.0.0.1:{self.dns}"],
                   env={"PEBBLE_VA_NOSLEEP": "1", "PEBBLE_WFE_NONCEREJECT": "0"})
        wait_for("Pebble", lambda: connects(self.acme) and connects(self.dns_admin))

        # deSEC here is the stand-in, and what it is told to publish goes
        # into Pebble's DNS. The token is minted as on the server, so the
        # renewal runs with the narrow one.
        def published(fqdn, values):
            def post(path, body):
                request = urllib.request.Request(f"http://127.0.0.1:{self.dns_admin}/{path}", data=json.dumps(body).encode())
                urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=5).read()
            post("clear-txt", {"host": fqdn})
            for value in values:
                post("set-txt", {"host": fqdn, "value": value})
        self.desec = Desec(published=published)
        server = self.desec.serve()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        api = f"http://127.0.0.1:{server.server_address[1]}/api/v1"
        os.makedirs(t("tls"))
        with open(t("cert.env"), "w") as file:
            file.write(f"CERT_NAME={NAME}\n")
        minted = subprocess.run(["python3", DESEC_PY, "mint", "--name", NAME, "--env", t("cert.env")], capture_output=True, text=True,
                                env={**os.environ, "DESEC_API": api,
                                     "DESEC_SETUP_TOKEN": self.desec.token("island-setup", manage=True, create=True)})
        self.assertEqual(minted.returncode, 0, minted.stdout + minted.stderr)
        with open(t("cert.env")) as file:
            token = dict(line.strip().split("=", 1) for line in file if "=" in line and not line.startswith("#"))["DESEC_TOKEN"]
        self.tls = t("tls")
        # As the unit gives it: the name and the token in the environment.
        self.env = {**os.environ, "CERT_NAME": NAME, "DESEC_TOKEN": token, "DESEC_API": api,
                    "CERT_ENV": t("cert.env"), "CERT_STATE": t("state"), "CERT_TLS": self.tls,
                    "CERT_LEGO": self.lego, "CERT_GROUP": str(os.getgid()),
                    "CERT_SERVER": f"https://localhost:{self.acme}/dir",
                    # Pebble's DNS is not deSEC's nameservers: lego is told where to ask.
                    "CERT_LEGO_EXTRA": f"--dns.resolvers 127.0.0.1:{self.dns} --dns.propagation.disable-ans",
                    "LEGO_CA_CERTIFICATES": t("ca.pem")}

    def spawn(self, argv, env=None):
        log = open(os.path.join(self.tmp, os.path.basename(argv[0]) + ".log"), "w")
        self.procs.append(subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT, env={**os.environ, **(env or {})}))
        return self.procs[-1]

    def stop(self):
        for proc in self.procs:
            proc.terminate()
        for proc in self.procs:
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()

    def issue(self, extra=""):
        env = {**self.env, "CERT_LEGO_EXTRA": self.env["CERT_LEGO_EXTRA"] + extra}
        done = subprocess.run([CERT_SH, "issue", "live"], env=env, capture_output=True, text=True, timeout=180)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        return done.stdout + done.stderr

    def serial(self, path):
        return int(openssl("x509", "-in", path, "-noout", "-serial").strip().removeprefix("serial="), 16)

    def served(self, port, roots):
        """The serial of the certificate flare-server hands a client that
        checks it as a browser would: the name, and the chain up to the root."""
        context = ssl.create_default_context(cafile=roots)
        with socket.create_connection(("127.0.0.1", port), timeout=5) as raw:
            with context.wrap_socket(raw, server_hostname=NAME) as tls:
                return int(tls.getpeercert()["serialNumber"], 16)

    def test_a_certificate_is_got_put_in_place_left_alone_and_renewed_under_a_running_server(self):
        cert, key = os.path.join(self.tls, "fullchain.pem"), os.path.join(self.tls, "privkey.pem")
        said = self.issue()
        self.assertIn(f"put the certificate in {self.tls}: for {NAME}", said)
        # The renewal wrote the proof record and took it away again, with
        # the narrow token, and wrote nothing else.
        wrote = [(who, method, path, status) for who, method, path, status in self.desec.writes if who == "island-cert"]
        self.assertEqual([(m, p.split("/api/v1")[1], c) for _, m, p, c in wrote[-2:]],
                         [("POST", f"/domains/{NAME}/rrsets/", 201), ("PATCH", f"/domains/{NAME}/rrsets/_acme-challenge/TXT/", 204)])
        self.assertEqual(set(self.desec.domains[NAME]), {("", "A")})
        for path in (cert, key):
            self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o640, path)
        self.assertIn("does match", openssl("x509", "-in", cert, "-noout", "-checkhost", NAME))
        self.assertEqual(sorted(os.listdir(self.tls)), ["fullchain.pem", "privkey.pem"])
        first, written = self.serial(cert), os.stat(cert).st_mtime_ns

        # A real flare-server on those files, as the unit starts it.
        run = lambda *a: subprocess.run([os.path.join(BIN, "island-cli"), *a], check=True, capture_output=True, text=True).stdout.strip()  # noqa: E731
        t = lambda name: os.path.join(self.tmp, name)  # noqa: E731
        for name in ("root", "appender"):
            run("keygen", t(name + ".key"))
        root = run("public", t("root.key"))
        run("init", "--log", t("island.log"), "--appender-key", t("appender.key"), "--root-key", t("root.key"))
        broker, plain, https = free_port(), free_port(), free_port()
        self.spawn([os.path.join(BIN, "island-broker"), "--log", t("island.log"), "--appender-key", t("appender.key"),
                    "--root", root, "--listen", f"127.0.0.1:{broker}"])
        wait_for("the broker", lambda: connects(broker))
        self.spawn([os.path.join(BIN, "flare-server"), "--broker", f"http://127.0.0.1:{broker}", "--root", root,
                    "--data", t("flare"), "--listen", f"127.0.0.1:{plain}", "--face", os.path.join(REPO, "face", "dist"),
                    "--tls-listen", f"127.0.0.1:{https}", "--tls-cert", cert, "--tls-key", key, "--allow-host", NAME])
        wait_for("flare-server", lambda: connects(https))
        roots = t("roots.pem")
        context = ssl.create_default_context(cafile=t("ca.pem"))
        with urllib.request.urlopen(f"https://localhost:{self.acme_admin}/roots/0", context=context, timeout=10) as answer, open(roots, "wb") as out:
            out.write(answer.read())
        self.assertEqual(self.served(https, roots), first)
        page = subprocess.run(["curl", "-s", "--noproxy", "*", "-o", "/dev/null", "-w", "%{http_code} %{content_type}", "--cacert", roots,
                               "--resolve", f"{NAME}:{https}:127.0.0.1", f"https://{NAME}:{https}/"],
                              capture_output=True, text=True).stdout
        self.assertEqual(page, "200 text/html; charset=utf-8")

        # The daily run, most days: nothing is due, nothing is touched.
        said = self.issue()
        self.assertIn("is the current one", said)
        self.assertEqual((self.serial(cert), os.stat(cert).st_mtime_ns), (first, written))

        # A renewal: new files, and the running server hands out the new one.
        said = self.issue(" --renew-force")
        self.assertIn(f"put the certificate in {self.tls}", said)
        second = self.serial(cert)
        self.assertNotEqual(second, first)
        self.assertEqual(sorted(os.listdir(self.tls)), ["fullchain.pem", "privkey.pem"])
        wait_for("flare-server to read the renewed certificate", lambda: self.served(https, roots) == second, 10)

    def test_a_staging_run_puts_nothing_in_place(self):
        env = {**self.env}
        done = subprocess.run([CERT_SH, "issue", "staging"], env=env, capture_output=True, text=True, timeout=180)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("it is not put anywhere", done.stdout)
        self.assertEqual(os.listdir(self.tls), [])
        self.assertTrue(re.search(r"\.crt$", " ".join(os.listdir(os.path.join(self.tmp, "state", "staging", "certificates"))), re.M))


if __name__ == "__main__":
    unittest.main()
