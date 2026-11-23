#!/usr/bin/env python3
"""desec.py: the deSEC side of the face's certificate (docs/server.md,
"The face over HTTPS").

The face's name is a zone of its own at deSEC, delegated to from the
domain's own DNS by two NS records. The server renews the certificate by
writing one TXT record there, `_acme-challenge.NAME`, and the token it
keeps for that can write nothing else: not the name's address, not
another record, no token, no domain. A stolen token can get a certificate
for the face's name and that is all; the domain's own DNS has no key on
the server at all.

  desec.py mint --name NAME --address 10.77.0.1 --env /etc/island/cert.env
      Asks for a deSEC token that may manage tokens (pasted, not shown,
      used once), makes the zone and its address record if they are
      missing, mints the narrow token, writes it to the env file, proves
      what it can and cannot do, and removes the pasted token if it is
      named `island-setup`.

  desec.py verify --name NAME --env /etc/island/cert.env [--write]
      Proves the kept token is narrow: it reads, and every write but the
      proof record's is refused. With --write it also writes and removes
      a proof record (not while a renewal runs).

  desec.py status --name NAME --env /etc/island/cert.env
      What deSEC sees of the delegation.

  desec.py present FQDN VALUE      desec.py cleanup FQDN VALUE
      What lego calls at a renewal (its `exec` DNS provider; cert.sh
      sets EXEC_PATH to this file): put the proof value into
      `_acme-challenge.NAME` TXT, and take it out again. The token and
      the name come from the environment, DESEC_TOKEN and CERT_NAME, as
      the unit gives them. These two are the only writes a renewal
      makes, and `mint` proves them against deSEC before any
      certificate is asked for.

For the tests: DESEC_API names another API (a stand-in), and
DESEC_SETUP_TOKEN gives `mint` its token without a terminal.
"""

from __future__ import annotations

import argparse
import getpass
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

API = os.environ.get("DESEC_API", "https://desec.io/api/v1").rstrip("/")
PROOF = "_acme-challenge"
KEPT = "island-cert"
SETUP = "island-setup"
NAMESERVERS = ("ns1.desec.io", "ns2.desec.org")


class Stop(Exception):
    """Something the person at the keyboard has to put right."""


class Api:
    def __init__(self, secret: str):
        self.secret = secret

    def call(self, method: str, path: str, body=None) -> tuple[int, object]:
        """One request. deSEC throttles (a few writes a second, some tens
        an hour) and says how long to wait: wait and ask again."""
        waited = 0.0
        while True:
            status, said, wait = self.once(method, path, body)
            if status != 429 or waited + wait > 180:
                return status, said
            time.sleep(wait)
            waited += wait

    def once(self, method: str, path: str, body) -> tuple[int, object, float]:
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(API + path, data=data, method=method)
        request.add_header("Authorization", "Token " + self.secret)
        request.add_header("Accept", "application/json")
        if data is not None:
            request.add_header("Content-Type", "application/json")
        wait = 1.0
        try:
            with urllib.request.urlopen(request, timeout=30) as answer:
                status, raw = answer.status, answer.read()
        except urllib.error.HTTPError as error:
            status, raw = error.code, error.read()
            try:
                wait = min(120.0, max(1.0, float(error.headers.get("Retry-After") or 1) + 0.5))
            except ValueError:
                pass
        except (urllib.error.URLError, OSError) as error:
            raise Stop(f"deSEC did not answer ({API}): {error}") from error
        try:
            said = json.loads(raw) if raw else None
        except ValueError:
            said = raw.decode(errors="replace")[:200]
        return status, said, wait


def say(text: str) -> None:
    print(text, flush=True)


def why(said) -> str:
    """deSEC's own words for a refusal, short."""
    if isinstance(said, dict) and "detail" in said:
        return str(said["detail"])
    return json.dumps(said)[:200] if said is not None else ""


def read_env(path: str) -> dict[str, str]:
    values: dict[str, str] = {}
    try:
        with open(path, encoding="utf-8") as file:
            for line in file:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    key, value = line.split("=", 1)
                    values[key] = value
    except FileNotFoundError:
        pass
    return values


def write_env(path: str, values: dict[str, str]) -> None:
    """The env file, whole, readable by root only, never half written."""
    directory = os.path.dirname(path) or "."
    handle, temporary = tempfile.mkstemp(dir=directory, prefix=".cert.env.")
    try:
        os.fchmod(handle, 0o600)
        with os.fdopen(handle, "w", encoding="utf-8") as file:
            file.write("# The face's certificate (scripts/node/cert.sh). Root only.\n")
            for key, value in values.items():
                file.write(f"{key}={value}\n")
        os.replace(temporary, path)
    except BaseException:
        if os.path.exists(temporary):
            os.unlink(temporary)
        raise


def kept_token(env: str) -> str:
    token = read_env(env).get("DESEC_TOKEN", "")
    if not token:
        raise Stop(f"{env} holds no DESEC_TOKEN yet: run `cert.sh token` first")
    return token


def proof_records(api: Api, name: str) -> list[str] | None:
    """The proof record's values now; None if there is no such record."""
    status, said = api.call("GET", f"/domains/{name}/rrsets/{PROOF}/TXT/")
    if status == 404:
        return None
    if status != 200 or not isinstance(said, dict):
        raise Stop(f"reading {PROOF}.{name} TXT: {status} {why(said)}")
    return list(said.get("records") or [])


def present(api: Api, name: str, value: str) -> None:
    """Add a value to the proof record (another may be there: two
    renewals, or a wildcard beside the name)."""
    quoted = json.dumps(value)
    records = proof_records(api, name)
    if records is None:
        status, said = api.call("POST", f"/domains/{name}/rrsets/", {"subname": PROOF, "type": "TXT", "ttl": 3600, "records": [quoted]})
        wanted = 201
    elif quoted in records:
        return
    else:
        status, said = api.call("PATCH", f"/domains/{name}/rrsets/{PROOF}/TXT/", {"records": records + [quoted]})
        wanted = 200
    if status != wanted:
        raise Stop(f"writing {PROOF}.{name} TXT: {status} {why(said)}")


def cleanup(api: Api, name: str, value: str) -> None:
    """Take a value out of the proof record; the record goes with its last value."""
    quoted = json.dumps(value)
    records = proof_records(api, name)
    if records is None or quoted not in records:
        return
    status, said = api.call("PATCH", f"/domains/{name}/rrsets/{PROOF}/TXT/", {"records": [r for r in records if r != quoted]})
    if status not in (200, 204):
        raise Stop(f"removing the value from {PROOF}.{name} TXT: {status} {why(said)}")


def challenge(what: str, fqdn: str, value: str) -> None:
    """lego's call: `present` or `cleanup`, for the one record this may write."""
    name, token = os.environ.get("CERT_NAME", ""), os.environ.get("DESEC_TOKEN", "")
    if not name or not token:
        raise Stop("CERT_NAME and DESEC_TOKEN must be in the environment (the unit reads them from /etc/island/cert.env)")
    if fqdn.rstrip(".").lower() != f"{PROOF}.{name}":
        raise Stop(f"asked to write {fqdn}; this writes {PROOF}.{name} and nothing else")
    (present if what == "present" else cleanup)(Api(token), name, value)
    if what == "present":
        published(name, value)


def published(name: str, value: str) -> None:
    """Wait until both of deSEC's nameservers answer with the value.

    deSEC takes a minute or two to publish a change, and its servers do
    not all switch at once: on 2026-11-23 Let's Encrypt was still handed
    the value of the run before. lego then waits a while longer before it
    asks Let's Encrypt to look (cert.sh). Not done against a stand-in."""
    if "DESEC_API" in os.environ or not shutil.which("dig"):
        return
    fqdn, end = f"{PROOF}.{name}", time.monotonic() + 420
    while time.monotonic() < end:
        answers = []
        for server in NAMESERVERS:
            try:
                answers.append(subprocess.run(["dig", "+short", "+norecurse", "TXT", fqdn, "@" + server],
                                              capture_output=True, text=True, timeout=20).stdout.split())
            except (OSError, subprocess.TimeoutExpired):
                answers.append([])
        if all(found == [json.dumps(value)] for found in answers):
            say(f"both nameservers answer with the new value for {fqdn}")
            return
        time.sleep(5)
    say(f"deSEC's nameservers did not both show the new value for {fqdn} in 7 minutes; going on")


def verify(name: str, token: str, write: bool) -> bool:
    """Whether the token is as narrow as it should be. Says each finding."""
    api = Api(token)
    good = True

    def finding(ok: bool, text: str) -> None:
        nonlocal good
        good = good and ok
        say(("PASS\t" if ok else "FAIL\t") + text)

    status, said = api.call("GET", f"/domains/{name}/rrsets/@/A/")
    if status == 401:
        finding(False, "deSEC knows the kept token (401: it does not; mint a new one with `cert.sh token`)")
        return False
    finding(status == 200, f"the kept token reads the name's address record ({status})")

    if write:
        # The two writes a renewal makes, as it makes them.
        value = "island-token-check"
        for done, act in ((f"it writes a value into the proof record, {PROOF}.{name} TXT", present), ("and takes it out again", cleanup)):
            try:
                act(api, name, value)
                finding(True, done)
            except Stop as stop:
                finding(False, f"{done} ({stop})")

    # Everything else must be refused. A write that goes through is undone.
    elsewhere = "island-scope-check"
    record = {"subname": elsewhere, "type": "TXT", "ttl": 3600, "records": ['"this should have been refused"']}
    status, _ = api.call("POST", f"/domains/{name}/rrsets/", record)
    if 200 <= status < 300:
        api.call("DELETE", f"/domains/{name}/rrsets/{elsewhere}/TXT/")
    finding(status == 403, f"a write to another record is refused ({status})")

    status, said = api.call("GET", f"/domains/{name}/rrsets/@/A/")
    if status == 200 and isinstance(said, dict):
        status, _ = api.call("PATCH", f"/domains/{name}/rrsets/@/A/", {"records": said["records"]})
        finding(status == 403, f"a write to the name's address is refused ({status})")

    status, _ = api.call("GET", "/auth/tokens/")
    finding(status == 403, f"it may not see or make tokens ({status})")
    return good


def mint(name: str, address: str, env: str) -> None:
    say("Paste a deSEC token that may manage tokens (make one named")
    say(f"`{SETUP}` in deSEC's web interface, with \"manage tokens\" ticked).")
    say("It is not shown here, is used once, and is not kept.")
    secret = os.environ.get("DESEC_SETUP_TOKEN") or getpass.getpass("deSEC setup token: ").strip()
    if not secret:
        raise Stop("nothing was pasted")
    api = Api(secret)

    status, said = api.call("GET", "/auth/tokens/")
    if status == 401:
        raise Stop("deSEC does not know that token (401). Copy it again, whole; it is shown only once, so make a new one if it is gone.")
    if status != 200 or not isinstance(said, list):
        raise Stop(f"that token may not manage tokens ({status} {why(said)}). Make one with \"manage tokens\" ticked.")
    tokens = said

    # The zone.
    status, said = api.call("GET", f"/domains/{name}/")
    if status == 404:
        status, said = api.call("POST", "/domains/", {"name": name})
        if status != 201:
            raise Stop(
                f"deSEC has no domain {name} in this account and the token may not create it ({status} {why(said)}).\n"
                f"Add the domain {name} in deSEC's web interface (Domain Management, the + button), then run this again."
            )
        say(f"== made the domain {name} at deSEC")
    elif status != 200:
        raise Stop(f"asking deSEC for the domain {name}: {status} {why(said)}")
    else:
        say(f"== deSEC has the domain {name}")
    ttl = max(3600, int(said.get("minimum_ttl", 3600))) if isinstance(said, dict) else 3600

    # The name's address: the tunnel address, which leads somewhere only inside the tunnel.
    status, said = api.call("GET", f"/domains/{name}/rrsets/@/A/")
    if status == 404:
        status, said = api.call("POST", f"/domains/{name}/rrsets/", {"subname": "", "type": "A", "ttl": ttl, "records": [address]})
        if status != 201:
            raise Stop(f"writing {name} A {address}: {status} {why(said)}")
        say(f"== wrote {name} A {address}")
    elif status == 200 and isinstance(said, dict) and said.get("records") == [address]:
        say(f"== {name} A {address} is there")
    elif status == 200:
        was = said.get("records") if isinstance(said, dict) else said
        status, said = api.call("PATCH", f"/domains/{name}/rrsets/@/A/", {"records": [address]})
        if status != 200:
            raise Stop(f"changing {name} A to {address}: {status} {why(said)}")
        say(f"== changed {name} A from {was} to {address}")
    else:
        raise Stop(f"reading {name} A: {status} {why(said)}")

    # The narrow token, in place of any made before.
    for token in tokens:
        if token.get("name") == KEPT:
            api.call("DELETE", f"/auth/tokens/{token['id']}/")
            say(f"== removed the older token named {KEPT}")
    status, said = api.call(
        "POST",
        "/auth/tokens/",
        {"name": KEPT, "perm_create_domain": False, "perm_delete_domain": False, "perm_manage_tokens": False},
    )
    if status != 201 or not isinstance(said, dict) or not said.get("token"):
        raise Stop(f"making the token {KEPT}: {status} {why(said)}")
    kept, kept_id = said["token"], said["id"]
    for flag in ("perm_create_domain", "perm_delete_domain", "perm_manage_tokens"):
        if said.get(flag):
            api.call("DELETE", f"/auth/tokens/{kept_id}/")
            raise Stop(f"deSEC made the token with {flag}; removed it again")
    policies = f"/auth/tokens/{kept_id}/policies/rrsets/"
    for policy in (
        {"domain": None, "subname": None, "type": None, "perm_write": False},
        {"domain": name, "subname": PROOF, "type": "TXT", "perm_write": True},
    ):
        status, said = api.call("POST", policies, policy)
        if status != 201:
            api.call("DELETE", f"/auth/tokens/{kept_id}/")
            raise Stop(f"limiting the token {KEPT}: {status} {why(said)}; removed it again")
    say(f"== made the token {KEPT}: it may write {PROOF}.{name} TXT and nothing else")

    values = read_env(env)
    values["CERT_NAME"] = name
    values["DESEC_TOKEN"] = kept
    write_env(env, values)
    say(f"== wrote it to {env} (root only)")

    if not verify(name, kept, write=True):
        raise Stop("the kept token is not what it should be; see the FAIL lines")

    # The pasted token has done its work.
    gone = False
    for token in tokens:
        if token.get("name") == SETUP:
            status, _ = api.call("DELETE", f"/auth/tokens/{token['id']}/")
            gone = gone or status == 204
    if gone:
        say(f"== removed the setup token {SETUP} at deSEC")
    else:
        say(f"NOTE\tthe pasted token is not named {SETUP}, so it was left: delete it in deSEC's web interface (Token Management) now")

    say("")
    say(f"Now, in the DNS of the domain above {name}, two records delegate the name to deSEC:")
    label = name.split(".", 1)[0]
    for server in NAMESERVERS:
        say(f"    type NS    name {label}    value {server}")
    say("Then: scripts/node/cert.sh dns")


def status(name: str, env: str) -> None:
    api = Api(kept_token(env))
    code, said = api.call("GET", f"/domains/{name}/status/")
    if code != 200 or not isinstance(said, dict):
        say(f"NOTE\tdeSEC's view of the delegation: no answer ({code})")
        return
    say(f"deSEC's view of {name}: {said.get('nameserver_status')} (nameservers seen at the parent: {', '.join(said.get('nameservers') or []) or 'none'}; checked {said.get('checked')})")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("what", choices=("mint", "verify", "status", "present", "cleanup"))
    parser.add_argument("challenge", nargs="*", help="present, cleanup: FQDN VALUE")
    parser.add_argument("--name")
    parser.add_argument("--address", default="10.77.0.1")
    parser.add_argument("--env", default="/etc/island/cert.env")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    try:
        if args.what in ("present", "cleanup"):
            if len(args.challenge) != 2:
                raise Stop(f"{args.what} FQDN VALUE")
            challenge(args.what, *args.challenge)
            return 0
        if not args.name:
            raise Stop("--name NAME")
        if args.what == "mint":
            mint(args.name, args.address, args.env)
        elif args.what == "verify":
            return 0 if verify(args.name, kept_token(args.env), args.write) else 1
        else:
            status(args.name, args.env)
    except Stop as stop:
        print(f"desec.py: {stop}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("\ndesec.py: stopped", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
