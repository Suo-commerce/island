#!/usr/bin/env bash
# install.sh: the Taso node on the Island server (slice 2, step 2;
# docs/server.md, "6. The Taso node").  Run as root, on the server, from
# the Island checkout:
#
#   TASO=/root/taso scripts/node/install.sh
#
# What it does, each step safe to run again:
#
#   1. builds Taso (taso-blkd, taso-wardend, taso-ctl, the Landlock shim),
#      island-sender, island-socket, flare-server, flare-agent and
#      flare-tui, release, and installs the services' programs in
#      /usr/local/bin, and the face (face/dist, built on the laptop and
#      kept in the repo) in /usr/local/share/flare/face;
#   2. builds the agents' image, /var/lib/taso/agent-island, without
#      Docker: flare-agent (step 4), bash and the everyday commands it
#      runs, python3 and its standard library (spike S4's stand-in agent
#      stays there for the spikes), the shim, and the mount targets.  A new
#      image goes in beside the old and is swapped in; a running agent
#      keeps the one it started on;
#   3. `taso-ctl init` (once): the module and expedited RCU at boot, the
#      directories, /etc/taso/warden.json;
#   4. writes flare-home's entry into /etc/taso/warden.json (replacing an
#      older one): it runs flare-agent. Checked by Taso's own parser
#      before it stays;
#   5. makes island-sender's key, /etc/island/sender.key, and flare-home's
#      writer key, /etc/island/vessels/<vessel-id>.key, if there are none,
#      and prints their public keys for the grants;
#   6. island-socket (step 3): its user, its key and config under
#      /etc/island-socket (written once; edited by hand after), a stub
#      model as its first provider, and the agent's instructions,
#      /etc/island-socket/instructions.md (written once; edited by hand
#      after, read at every task);
#   7. installs the units taso-blkd, taso-wardend, island-sender,
#      island-socket, island-stub-model and flare-server (with its agent
#      socket, step 4) and enables them.  It starts nothing: that is the
#      last command it prints.
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
SOCKET_KEY=/etc/island-socket/socket.key
SOCKET_CONFIG=/etc/island-socket/config.toml
NS=151a4d00000000000000000000000052

say() { echo "== $*"; }
die() { echo "install.sh: $*" >&2; exit 2; }

[ "$(id -u)" = 0 ] || die "run as root"
for t in runc ldd python3 cargo systemctl useradd runuser; do command -v "$t" >/dev/null || die "$t is missing"; done
[ -f "$TASO/Cargo.toml" ] || die "no Taso checkout at $TASO (set TASO)"
[ -f "$ENV" ] || die "$ENV is missing: the broker is not set up (docs/server.md, 3)"
grep -q '^ISLAND_ROOT=[0-9a-f]\{64\}$' "$ENV" || die "$ENV has no ISLAND_ROOT"

# ── 1. programs ────────────────────────────────────────────────────────
say "building Taso (release) at $(git -C "$TASO" log -1 --format='%h %s' 2>/dev/null || echo '?')"
(cd "$TASO" && cargo build --release -q -p taso-warden -p taso-landlock \
    && cargo build --release -q -p taso-blk --bin taso-blkd)
say "building island-sender, island-socket, island-cli, flare-server, flare-agent and flare-tui (release) at $(git -C "$ISLAND" log -1 --format='%h' 2>/dev/null || echo '?')"
(cd "$ISLAND" && cargo build --release -q --locked -p island-sender -p island-socket -p island-cli \
    -p flare-server -p flare-agent -p flare-tui)
install -m 755 "$TASO/target/release/taso-blkd" "$TASO/target/release/taso-wardend" \
    "$TASO/target/release/taso-ctl" "$ISLAND/target/release/island-sender" \
    "$ISLAND/target/release/island-socket" "$ISLAND/target/release/flare-server" /usr/local/bin/
say "installed taso-blkd, taso-wardend, taso-ctl, island-sender, island-socket, flare-server in /usr/local/bin"
# The face: the page flare-server serves. It is built on the laptop and
# kept in the repo (face/dist), so nothing of Node runs on this machine.
[ -f "$ISLAND/face/dist/index.html" ] || die "$ISLAND/face/dist has no index.html: the built face is missing from the checkout"
mkdir -p /usr/local/share/flare
rm -rf /usr/local/share/flare/face.new
cp -r "$ISLAND/face/dist" /usr/local/share/flare/face.new
chmod -R u=rwX,go=rX /usr/local/share/flare/face.new
rm -rf /usr/local/share/flare/face.old
[ ! -e /usr/local/share/flare/face ] || mv /usr/local/share/flare/face /usr/local/share/flare/face.old
mv /usr/local/share/flare/face.new /usr/local/share/flare/face
rm -rf /usr/local/share/flare/face.old
say "installed the face in /usr/local/share/flare/face"
if ! id island-socket >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin island-socket
    say "made the user island-socket"
fi

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
# A command as the host has it: a link (one multi-call binary, as
# Ubuntu's coreutils is) stays a link to one copy of its target.
copy_command() {   # $1 command name
    local path real
    # The file, not a shell builtin of the same name (echo, printf).
    path=$(type -P "$1") || return 0
    real=$(readlink -f "$path")
    [ -e "$NEW$real" ] || copy_with_libs "$real" "$real"
    if [ "$real" != "/usr/bin/$1" ]; then
        mkdir -p "$NEW/usr/bin"
        ln -sf "$real" "$NEW/usr/bin/$1"
    fi
}
copy_with_libs "$ISLAND/target/release/flare-agent" /usr/bin/flare-agent
for c in bash sh ls cat head tail wc grep sed awk find sort uniq mkdir rmdir rm mv cp ln touch \
    date env echo printf diff tr cut xargs sleep seq tee basename dirname realpath stat du rg; do
    copy_command "$c"
done
mkdir -p "$NEW/bin"
[ -e "$NEW/bin/sh" ] || ln -s /usr/bin/sh "$NEW/bin/sh"
[ -e "$NEW/bin/bash" ] || ln -s /usr/bin/bash "$NEW/bin/bash"
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
# island-socket places its socket here; the agents may only connect.
chown island-socket:island-socket "$NEW/run/island"
chmod 755 "$NEW/run/island"
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
# flare-agent (step 4): it takes tasks from the session, through the
# island socket, and replies through island-sender.  What it keeps lives
# in its /work (.flare/), so a restart or a reboot carries on from the
# last Seal.
cp "$CONFIG" "$CONFIG.before-flare-home"
python3 - "$CONFIG" "$NS" "$IMAGE" <<'PY'
import json, os, sys
path, ns, image = sys.argv[1:4]
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
        "args": ["/usr/bin/flare-agent"],
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
rc=0
taso-ctl status "$CONFIG" >/tmp/taso-status.out 2>&1 || rc=$?
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

# flare-home's own writer key (step 5): island-sender signs its approved
# verses with it. Root only, one file per Vessel, named by its id.
FLARE_HOME_ID=$("$ISLAND/target/release/island-socket" vessels --warden-config "$CONFIG" \
    | awk '$2 == "flare-home" && $3 == "prime" { print $1 }')
[ -n "$FLARE_HOME_ID" ] || die "no flare-home in $CONFIG"
install -d -m 700 /etc/island/vessels
VESSEL_KEY=/etc/island/vessels/$FLARE_HOME_ID.key
if [ ! -f "$VESSEL_KEY" ]; then
    (umask 077; "$ISLAND/target/release/island-cli" keygen "$VESSEL_KEY" >/dev/null)
    say "made $VESSEL_KEY (flare-home's writer key)"
fi
chmod 600 "$VESSEL_KEY"
VESSEL_PUB=$("$ISLAND/target/release/island-cli" public "$VESSEL_KEY")

# ── 6. island-socket ───────────────────────────────────────────────────
# Its own user, its config and key under /etc/island-socket (the user
# reads them; only root writes), and the stub model as its first provider
# until a real one is configured (docs/server.md, 7).
install -D -m 755 "$ISLAND/scripts/s3/stub-model.py" /usr/local/lib/island/stub-model.py
install -d -m 750 -o root -g island-socket /etc/island-socket
if [ ! -f "$SOCKET_KEY" ]; then
    (umask 077; "$ISLAND/target/release/island-cli" keygen "$SOCKET_KEY" >/dev/null)
    say "made $SOCKET_KEY"
fi
chown island-socket:island-socket "$SOCKET_KEY"
chmod 600 "$SOCKET_KEY"
SOCKET_PUB=$("$ISLAND/target/release/island-cli" public "$SOCKET_KEY")
if [ ! -f "$SOCKET_CONFIG" ]; then
    printf '%s\n' \
        "# island-socket (docs/slice-2.md, \"Providers\"; docs/server.md, 7)." \
        "socket = \"$IMAGE/run/island/island.sock\"" \
        "warden_config = \"$CONFIG\"" \
        "state_dir = \"/var/lib/island-socket\"" \
        "broker = \"http://10.77.0.1:4747\"" \
        "root = \"$(sed -n 's/^ISLAND_ROOT=//p' "$ENV")\"" \
        "key = \"$SOCKET_KEY\"" \
        "access_every_secs = 30" \
        "default = \"stub\"" \
        "" \
        "# A placeholder: island-stub-model.service, filler text. Replace it" \
        "# with real providers, then point default at one of them." \
        "[provider.stub]" \
        "base_url = \"http://127.0.0.1:8099/v1\"" \
        "models = [\"stub\"]" \
        "budget = { calls = 1000, bytes = 100000000 }" >"$SOCKET_CONFIG"
    chown root:island-socket "$SOCKET_CONFIG"
    chmod 640 "$SOCKET_CONFIG"
    say "wrote $SOCKET_CONFIG (the stub provider only)"
fi
# The agent's instructions: written once, then edited by hand; island-socket
# reads them at every task, so an edit needs no restart.
INSTRUCTIONS=/etc/island-socket/instructions.md
if [ ! -f "$INSTRUCTIONS" ]; then
    install -m 640 -o root -g island-socket "$ISLAND/deploy/server/agent-instructions.md" "$INSTRUCTIONS"
    say "wrote $INSTRUCTIONS (the agent's instructions; edit them there)"
fi
# It places its socket in the image, through /var/lib/taso.
runuser -u island-socket -- test -w "$IMAGE/run/island" \
    || die "island-socket cannot write $IMAGE/run/island (a directory above it is closed to it)"
# It reads the Vessels from Taso's config.
if ! runuser -u island-socket -- test -r "$CONFIG"; then
    chmod 644 "$CONFIG"
    say "made $CONFIG readable: island-socket reads who the Vessels are from it"
fi

# ── 7. the units ───────────────────────────────────────────────────────
install -m 644 "$ISLAND/deploy/server/taso-blkd.service" "$ISLAND/deploy/server/taso-wardend.service" \
    "$ISLAND/deploy/server/island-sender.service" "$ISLAND/deploy/server/island-socket.service" \
    "$ISLAND/deploy/server/island-stub-model.service" "$ISLAND/deploy/server/flare-server.service" \
    /etc/systemd/system/
install -D -m 755 "$ISLAND/deploy/server/taso-unmount-stale" /usr/local/lib/island/taso-unmount-stale
systemctl daemon-reload
systemctl enable -q taso-blkd taso-wardend island-sender island-socket island-stub-model flare-server
say "units installed and enabled (not started): taso-blkd, taso-wardend, island-sender, island-socket, island-stub-model, flare-server"
# The face's certificate, where scripts/node/cert.sh has set it up: the
# renewal's programs and units follow the checkout too.
if [ -f /etc/island/cert.env ]; then "$ISLAND/scripts/node/cert.sh" units; fi

cat <<END

Next:

1. On the laptop, the grants each key needs, if it has none yet:
   island-sender  $SENDER_PUB  'dispatch' on 'approvals'
   island-socket  $SOCKET_PUB  'access' on 'access'
   flare-home     $VESSEL_PUB  'verse' on 'flare'
2. Restart, every time this script has run:
   systemctl restart flare-server island-stub-model island-socket island-sender
   systemctl restart taso-wardend
   The Warden's restart is not optional: this script put a new image in the
   old one's place, and the island socket moved with it. A flare-home that
   keeps running has the old image, cannot reach the socket, and takes no
   task until it starts again. (First install: systemctl start taso-blkd
   taso-wardend.)
3. Check it:        scripts/node/check.sh
   Talk to it:      scripts/node/check.sh converse
END
