#!/usr/bin/env python3
"""Spike S4's agent: runs inside a Taso Vessel, as the prime or as a fork
child of it, and carries on where its last Seal left it.

    s4-agent.py --prime STEP,STEP,... --fork STEP,STEP,...

A fork child runs the same image and command line as its parent, so the
agent asks the island socket's stand-in (s4-host.py) who it is, and runs
the plan for its role.  Where it is in that plan lives in /work, in
/work/.s4/<role>.next, written before every Seal: a start after a crash
or a promotion carries on from what the Seal holds, as flare-agent will.
Everything it does is also sent to the host's journal, which no rollback
or promotion can touch: that is what the spike's checks read.

Until flare-agent exists (slice 2, step 4), the node's long-lived Vessel
flare-home runs this agent too, with --island '' --role prime: no island
socket yet, and the journal goes to standard error and to --journal-file
in /work, where the host can read it.

Steps:
    seal              Transcript + TurnEnd; the time it took is journalled
    mark:NAME         write /work/NAME (who wrote it, in which turn)
    rm:NAME           remove /work/NAME
    ls                journal the names in /work (and what each mark says)
    bulk:FILES:MIB    write FILES files, MIB MiB in all, under /work/bulk
    touch:N           rewrite the first N of them
    effect:irr:TAG    declare an Irreversible effect; key and request are
    effect:nem:TAG    derived from TAG, so a redone turn declares the same
                      key; its op is kept on the host under TAG
    result:TAG        ask Taso what became of the op kept under TAG
    fork              ask for a child from the newest Seal
    promote           adopt the child the last fork made; Taso ends the run
    waitnote:N[:S]    wait until the host holds N notes from forks; with S,
                      at most S seconds, then go on
    note:TEXT         post a note to the parent, through the host
    setprime:N        write the prime's place in its plan (a child does
                      this, so a prime started on its files goes on at N)
    idle              sleep until killed (the end of a long-lived plan)
    exit              end the run (a fork child's last step)
"""

import argparse
import hashlib
import json
import os
import socket
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import s2_field as f  # noqa: E402


def log(msg):
    print(f"s4-agent: {msg}", file=sys.stderr, flush=True)


class Island:
    """The island socket's stand-in: one JSON line each way."""

    def __init__(self, path):
        self.path = path
        self.connect()

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(self.path)
        self.reader = self.sock.makefile("r")

    def ask(self, **req):
        # The socket's server may restart: one new connection, then give up.
        for attempt in (1, 2):
            try:
                self.sock.sendall((json.dumps(req) + "\n").encode())
                line = self.reader.readline()
                if line:
                    return json.loads(line)
                raise ConnectionError("the island socket closed")
            except OSError:
                if attempt == 2:
                    raise
                self.sock.close()
                time.sleep(0.5)
                self.connect()


class Agent:
    def __init__(self, args):
        self.args = args
        if args.island:
            self.island = Island(args.island)
            me = self.island.ask(op="whoami")
            self.role, self.slot = me["role"], me["slot"]
        else:
            # No island socket yet (the node before step 3): the role is
            # given, and the journal goes to standard error.
            self.island = None
            self.role, self.slot = args.role, None
        self.start = uuid.uuid4().hex[:8]
        self.field = f.Field(args.socket)
        self.last_sealed = self.field.hello["last_sealed_turn"]
        self.turn = (self.last_sealed or 0) + 1
        self.index = 0
        self.state = os.path.join(args.workdir, ".s4")
        os.makedirs(self.state, exist_ok=True)
        self.steps = [s for s in (args.prime if self.role == "prime" else args.fork).split(",") if s]
        self.at = self.read_int(f"{self.role}.next", 0)
        self.journal("start", role=self.role, last_sealed=self.last_sealed, resume_at=self.at,
                     files=self.names())
        log(f"{self.role} in slot {self.slot}: last sealed turn {self.last_sealed}, step {self.at}")

    # ── helpers ──────────────────────────────────────────────────────

    def journal(self, step, **fields):
        event = {"step": step, "start": self.start, "role": self.role, "turn": self.turn,
                 "t": round(time.time(), 3), **fields}
        if self.island is None:
            line = json.dumps(event, sort_keys=True)
            log(line)
            if self.args.journal_file:
                # In /work: the host reads it where the Warden mounts the
                # dataset. What comes after the last Seal goes at the next
                # start, like every file there.
                with open(self.args.journal_file, "a") as out:
                    out.write(line + "\n")
        else:
            self.island.ask(op="log", event=event)

    def host(self, **req):
        if self.island is None:
            raise RuntimeError(f"{req['op']} needs the island socket (--island)")
        return self.island.ask(**req)

    def path(self, name):
        return os.path.join(self.args.workdir, name)

    def read_int(self, name, default):
        try:
            with open(os.path.join(self.state, name)) as fh:
                return int(fh.read().strip())
        except (OSError, ValueError):
            return default

    def write(self, path, text):
        tmp = path + ".tmp"
        with open(tmp, "w") as out:
            out.write(text)
        os.replace(tmp, path)

    def names(self):
        out = {}
        for name in sorted(os.listdir(self.args.workdir)):
            if name.startswith(".") or name in ("bulk", "lost+found"):
                continue
            p = self.path(name)
            try:
                with open(p) as fh:
                    out[name] = json.loads(fh.read())
            except (OSError, ValueError):
                out[name] = None
        return out

    def key(self, tag):
        return hashlib.sha256(f"s4:{tag}".encode()).digest()[:16]

    # ── steps ────────────────────────────────────────────────────────

    def seal(self, at):
        # The place to carry on from goes into the Seal itself.
        self.write(os.path.join(self.state, f"{self.role}.next"), str(at + 1))
        t0 = time.monotonic()
        r = self.field.ask(f.transcript(f"s4: {self.role} turn {self.turn}".encode()))
        if r.get("type") != "Recorded":
            raise RuntimeError(f"unexpected answer to the transcript: {r}")
        r = self.field.ask(f.turn_end(self.turn, True))
        if r.get("type") != "TurnEnd":
            raise RuntimeError(f"unexpected answer to TurnEnd: {r}")
        self.journal("seal", sealed=r["sealed"], seconds=round(time.monotonic() - t0, 3))
        self.turn += 1
        self.index = 0

    def mark(self, name):
        self.write(self.path(name), json.dumps({"by": self.role, "slot": self.slot, "turn": self.turn}))

    def bulk(self, files, mib):
        t0 = time.monotonic()
        size = max(1, (mib << 20) // files)
        for i in range(files):
            d = self.path(f"bulk/d{i // 256:03d}")
            os.makedirs(d, exist_ok=True)
            with open(os.path.join(d, f"f{i:05d}"), "wb") as out:
                out.write(os.urandom(size))
        self.journal("bulk", files=files, bytes=files * size, seconds=round(time.monotonic() - t0, 3))

    def touch(self, n):
        t0 = time.monotonic()
        for i in range(n):
            p = self.path(f"bulk/d{i // 256:03d}/f{i:05d}")
            size = os.path.getsize(p)
            with open(p, "wb") as out:
                out.write(os.urandom(size))
        self.journal("touch", files=n, seconds=round(time.monotonic() - t0, 3))

    def effect(self, kind, tag):
        consequence = {"nem": f.NO_EXTERNAL_MUTATION, "irr": f.IRREVERSIBLE}[kind]
        call = self.turn * 1000 + self.index
        self.index += 1
        reply = self.field.ask(f.effect(call, "island", f"s4 {tag}".encode(), self.key(tag),
                                        consequence, authority=b"s4"))
        if reply.get("type") != "Effect":
            raise RuntimeError(f"unexpected answer to effect {tag}: {reply}")
        if "op" in reply and self.island is not None:
            self.host(op="put", key=f"op:{self.slot}:{tag}", value=reply["op"])
        self.journal("effect", tag=tag, key=self.key(tag).hex(), result=reply["result"],
                     op=reply.get("op"), reason=reply.get("reason"))

    def result(self, tag):
        op = self.host(op="get", key=f"op:{self.slot}:{tag}")["value"]
        if op is None:
            self.journal("result", tag=tag, state="no op recorded")
            return
        r = self.field.ask(f.result(op))
        self.journal("result", tag=tag, op=op, state=r.get("state"), reason=r.get("reason"))

    def fork(self):
        # A fork needs its Seal shipped first; Taso ships right after the
        # Seal, so a refusal for that is tried again, as an agent would.
        t0 = time.monotonic()
        refusals = []
        for _ in range(self.args.fork_tries):
            r = self.field.ask(f.fork())
            if r.get("result") == "Forked":
                self.write(os.path.join(self.state, "child"), r["child"])
                break
            refusals.append(r.get("reason"))
            if "slot" in (r.get("reason") or ""):
                break  # no slot left: trying again changes nothing
            time.sleep(1)
        self.journal("fork", result=r.get("result"), child=r.get("child"), seal=r.get("seal"),
                     reason=r.get("reason"), refusals=refusals,
                     seconds=round(time.monotonic() - t0, 3))

    def promote(self):
        try:
            with open(os.path.join(self.state, "child")) as fh:
                child = fh.read().strip()
        except OSError:
            self.journal("promote", result="no child to promote")
            return
        t0 = time.monotonic()
        r = self.field.ask(f.promote(child))
        self.journal("promote", child=child, result=r.get("result"), seal=r.get("seal"),
                     adopted=r.get("adopted"), reason=r.get("reason"),
                     seconds=round(time.monotonic() - t0, 3))
        if r.get("result") != "Promoted":
            return
        # Taso ends the run for the adoption: wait for that, and say so
        # if it never comes.
        deadline = time.monotonic() + self.args.promote_wait
        while time.monotonic() < deadline:
            time.sleep(0.5)
        self.journal("promote-run-not-ended", seconds=self.args.promote_wait)
        sys.exit(0)

    def waitnote(self, n, secs=None):
        """Wait for n notes; with secs, give up after that long and go on."""
        deadline = time.monotonic() + (secs if secs is not None else self.args.wait_timeout)
        while True:
            notes = self.host(op="notes")["notes"]
            if len(notes) >= n:
                self.journal("notes", notes=notes)
                return
            if time.monotonic() > deadline:
                if secs is not None:
                    self.journal("notes-timeout", wanted=n, notes=notes)
                    return
                raise TimeoutError(f"no {n} note(s) after {self.args.wait_timeout} s")
            time.sleep(0.2)

    def idle(self):
        self.journal("idle")
        while True:
            time.sleep(3600)

    # ── driver ───────────────────────────────────────────────────────

    def run(self):
        while self.at < len(self.steps):
            step = self.steps[self.at]
            name, _, rest = step.partition(":")
            log(f"step {self.at}: {step}")
            if name == "seal":
                self.seal(self.at)
            elif name == "mark":
                self.mark(rest)
            elif name == "rm":
                if os.path.exists(self.path(rest)):
                    os.remove(self.path(rest))
            elif name == "ls":
                self.journal("ls", files=self.names())
            elif name == "bulk":
                files, _, mib = rest.partition(":")
                self.bulk(int(files), int(mib))
            elif name == "touch":
                self.touch(int(rest))
            elif name == "effect":
                kind, _, tag = rest.partition(":")
                self.effect(kind, tag)
            elif name == "result":
                self.result(rest)
            elif name == "fork":
                self.fork()
            elif name == "promote":
                self.promote()
            elif name == "waitnote":
                n, _, secs = rest.partition(":")
                self.waitnote(int(n), float(secs) if secs else None)
            elif name == "note":
                self.host(op="note", text=rest)
            elif name == "setprime":
                self.write(os.path.join(self.state, "prime.next"), rest)
            elif name == "idle":
                self.idle()
            elif name == "exit":
                break
            else:
                raise ValueError(f"unknown step {step!r}")
            self.at += 1
        self.journal("exit", at=self.at)
        self.field.close()


KNOWN = {"seal", "mark", "rm", "ls", "bulk", "touch", "effect", "result", "fork", "promote",
         "waitnote", "note", "setprime", "idle", "exit"}


def main():
    p = argparse.ArgumentParser(description="spike S4 agent (inside a Vessel)")
    p.add_argument("--prime", required=True, help="the prime's steps, comma-separated")
    p.add_argument("--fork", required=True, help="a fork child's steps, comma-separated")
    p.add_argument("--socket", default="/run/taso/field.sock")
    p.add_argument("--island", default="/run/island/island.sock",
                   help="the island socket; empty: none, and --role says who this is")
    p.add_argument("--role", choices=["prime", "fork"], default="prime",
                   help="with --island '': the role, which whoami would tell")
    p.add_argument("--journal-file", default="",
                   help="with --island '': also append the journal to this file")
    p.add_argument("--workdir", default="/work")
    p.add_argument("--wait-timeout", type=float, default=1800.0)
    p.add_argument("--promote-wait", type=float, default=120.0)
    p.add_argument("--fork-tries", type=int, default=30)
    args = p.parse_args()
    for plan in (args.prime, args.fork):
        for s in plan.split(","):
            if s and s.partition(":")[0] not in KNOWN:
                p.error(f"unknown step {s!r}")
    Agent(args).run()


if __name__ == "__main__":
    main()
