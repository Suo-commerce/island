#!/usr/bin/env bash
# install.sh: the Taso node on the Island server (slice 2, step 2;
# docs/server.md, "6. The Taso node").  Run as root, on the server, from
# the Island checkout:
#
#   TASO=/root/taso scripts/node/install.sh
#
# What it does, each step safe to run again:
#
#   1. builds Taso (taso-blkd, taso-wardend, taso-ctl, the Landlock shim)
#      and island-sender, release, and installs them in /usr/local/bin;
#   2. builds the agents' image, /var/lib/taso/agent-island, without
#      Docker: python3 and its standard library, the stand-in agent
#      (scripts/s2/s4-agent.py) until flare-agent exists, the shim, and the
#      mount targets.  A new image goes in beside the old and is swapped
#      in; a running agent keeps the one it started on;
#   3. `taso-ctl init` (once): the module and expedited RCU at boot, the
#      directories, /etc/taso/warden.json;
#   4. writes flare-home's entry into /etc/taso/warden.json (replacing an
#      older one), checked by Taso's own parser before it stays;
#   5. makes island-sender's key, /etc/island/sender.key, if there is none,
#      and prints its public key for the grant;
#   6. installs the units taso-blkd, taso-wardend and island-sender and
#      enables them.  It starts nothing: that is the last command it prints.
#
# Environment: TASO (/root/taso)  SIZE_GIB (32: the block log, at init only)
#
# It opens no port and adds no firewall rule; Taso adds none either.

set -euo pipefail
trap 'echo "STOPPED at line $LINENO (exit $?)" >&2' ERR

TASO=${TASO:-/root/taso}
SIZE_GIB=${SIZE_GIB:-32}
ISLAND=$(cd "$(dirname "$0")/../.." && pwd)
S2=$ISLAND/scripts/s2
DATA=/var/lib/taso
IMAGE=$DATA/agent-island
CONFIG=/etc/taso/warden.json
ENV=/etc/island/island.env
SENDER_KEY=/etc/island/sender.key
NS=151a4d00000000000000000000000052

say() { echo "== $*"; }
die() { echo "install.sh: $*" >&2; exit 2; }

[ "$(id -u)" = 0 ] || die "run as root"
for t in runc ldd python3 cargo systemctl; do command -v "$t" >/dev/null || die "$t is missing"; done
[ -f "$TASO/Cargo.toml" ] || die "no Taso checkout at $TASO (set TASO)"
[ -f "$ENV" ] || die "$ENV is missing: the broker is not set up (docs/server.md, 3)"
grep -q '^ISLAND_ROOT=[0-9a-f]\{64\}$' "$ENV" || die "$ENV has no ISLAND_ROOT"

# ── 1. programs ────────────────────────────────────────────────────────
say "building Taso (release) at $(git -C "$TASO" log -1 --format='%h %s' 2>/dev/null || echo '?')"
(cd "$TASO" && cargo build --release -q -p taso-warden -p taso-landlock \
    && cargo build --release -q -p taso-blk --bin taso-blkd)
say "building island-sender and island-cli (release) at $(git -C "$ISLAND" log -1 --format='%h' 2>/dev/null || echo '?')"
(cd "$ISLAND" && cargo build --release -q --locked -p island-sender -p island-cli)
install -m 755 "$TASO/target/release/taso-blkd" "$TASO/target/release/taso-wardend" \
    "$TASO/target/release/taso-ctl" "$ISLAND/target/release/island-sender" /usr/local/bin/
say "installed taso-blkd, taso-wardend, taso-ctl, island-sender in /usr/local/bin"

# ── 2. the image ───────────────────────────────────────────────────────
NEW=$IMAGE.new
rm -rf "$NEW"
mkdir -p "$NEW"
copy_with_libs() {   # $1 host file -> $2 path in the image, with its libraries
    install -D -m 755 "$1" "$NEW$2"
    local lib
    for lib in $(ldd "$1" 2>/dev/null | grep -o '/[^ ]*' || true); do
        [ -f "$lib" ] && install -D -m 755 "$lib" "$NEW$lib"
    done
}
copy_with_libs "$(command -v python3)" /usr/bin/python3
copy_with_libs "$TASO/target/release/taso-landlock" /usr/lib/taso/landlock
PY_LIB=$(python3 -c 'import os; print(os.path.dirname(os.__file__))')
mkdir -p "$NEW$(dirname "$PY_LIB")"
cp -a "$PY_LIB" "$NEW$PY_LIB"
find "$NEW$PY_LIB" -name __pycache__ -prune -exec rm -rf {} +
for lib in $(find "$NEW$PY_LIB" -name '*.so' -exec ldd {} + 2>/dev/null | grep -o '/[^ ]*' | sort -u || true); do
    [ -f "$NEW$lib" ] || install -D -m 755 "$lib" "$NEW$lib" 2>/dev/null || true
done
install -D -m 644 "$S2/s2_field.py" "$NEW/usr/lib/flare/s2_field.py"
install -D -m 755 "$S2/s4-agent.py" "$NEW/usr/lib/flare/s4-agent.py"
mkdir -p "$NEW"/{proc,dev,sys,tmp,work,run/taso,run/island,etc}
: > "$NEW/run/taso/field.sock"
sed -n 's/^ISLAND_ROOT=//p' "$ENV" > "$NEW/etc/island-root.pub"
git -C "$ISLAND" log -1 --format='island %H' > "$NEW/etc/flare-image"
git -C "$TASO" log -1 --format='taso %H' >> "$NEW/etc/flare-image"
if [ -d "$IMAGE" ]; then
    rm -rf "$IMAGE.old"
    mv "$IMAGE" "$IMAGE.old"
fi
mkdir -p "$DATA"
mv "$NEW" "$IMAGE"
say "image $IMAGE ($(du -sh "$IMAGE" | cut -f1)); the previous one, if any, is $IMAGE.old"

# ── 3. taso-ctl init ───────────────────────────────────────────────────
say "taso-ctl init"
taso-ctl init --data "$DATA" --size-gib "$SIZE_GIB" --rootfs "$IMAGE"

# ── 4. flare-home ──────────────────────────────────────────────────────
# Until flare-agent (step 4), the stand-in agent: one turn, one action to
# approve, then it waits.  Its place in that plan lives in its /work, so a
# restart or a reboot carries on from the last Seal and never proposes the
# action twice.
PLAN="mark:flare-home,seal,effect:irr:hello-from-flare-home,seal,idle"
cp "$CONFIG" "$CONFIG.before-flare-home"
python3 - "$CONFIG" "$NS" "$IMAGE" "$PLAN" <<'PY'
import json, os, sys
path, ns, image, plan = sys.argv[1:5]
config = json.load(open(path))
entry = {
    "namespace": ns,
    "subject": "flare-home",
    "uid_slot": 10,
    "effects": "dispatch",
    "restart": "always",
    "forks": {"slots": [11, 12, 13, 14, 15, 16, 17, 18]},
    "runtime": {
        "rootfs": image,
        "args": ["/usr/bin/python3", "/usr/lib/flare/s4-agent.py", "--island", "", "--role", "prime",
                 "--journal-file", "/work/.flare-journal.jsonl", "--prime", plan, "--fork", "exit"],
        "memory_bytes": 2147483648,
        "pids": 256,
    },
}
others = [v for v in config["vessels"] if v.get("subject") != "flare-home"]
had = len(others) != len(config["vessels"])
config["vessels"] = others + [entry]
tmp = path + ".tmp"
with open(tmp, "w") as out:
    json.dump(config, out, indent=2)
    out.write("\n")
os.replace(tmp, path)
print("  flare-home's entry " + ("replaced" if had else "added"))
PY
set +e
taso-ctl status "$CONFIG" >/tmp/taso-status.out 2>&1
rc=$?
set -e
if [ "$rc" = 2 ]; then
    cp "$CONFIG.before-flare-home" "$CONFIG"
    cat /tmp/taso-status.out >&2
    die "Taso refuses the config with flare-home in it; the old one is back"
fi
say "Taso's parser takes $CONFIG (taso-ctl status: exit $rc, 1 is only 'the block log does not answer')"

# ── 5. island-sender's key ─────────────────────────────────────────────
if [ ! -f "$SENDER_KEY" ]; then
    (umask 077; "$ISLAND/target/release/island-cli" keygen "$SENDER_KEY" >/dev/null)
    say "made $SENDER_KEY"
fi
chmod 600 "$SENDER_KEY"
SENDER_PUB=$("$ISLAND/target/release/island-cli" public "$SENDER_KEY")

# ── 6. the units ───────────────────────────────────────────────────────
install -m 644 "$ISLAND/deploy/server/taso-blkd.service" "$ISLAND/deploy/server/taso-wardend.service" \
    "$ISLAND/deploy/server/island-sender.service" /etc/systemd/system/
systemctl daemon-reload
systemctl enable -q taso-blkd taso-wardend island-sender
say "units installed and enabled (not started): taso-blkd, taso-wardend, island-sender"

cat <<END

Next:

1. On the laptop, grant island-sender's key 'dispatch' on 'approvals'
   (its public key: $SENDER_PUB).
2. Start the node:  systemctl start taso-blkd taso-wardend island-sender
3. Check it:        scripts/node/check.sh
END
