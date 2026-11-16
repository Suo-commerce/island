#!/usr/bin/env bash
# s2-spike.sh: spikes S2, S4 and S5, and the server gate of slice 2
# (`just vessel-gate`). An Island agent in Taso Vessels on this host,
# checked against what the S2 plan read in Taso's code.
#
#   a  the prime Vessel dispatches: 3 effects released, a sender applies
#      them, none listed twice
#   b  a fork child holds: its sender socket lists nothing
#   g  ... even for a NoExternalMutation effect: answered held, never
#      released (a child cannot ask a model through the Field)
#   h  a Unix socket placed in the Vessels' read-only root filesystem
#      (the inference socket of decision D3) is reached by the prime and
#      by a fork child, 1 MiB each way; the Vessel has no TCP to the host
#   c  the prime promotes a child: exactly the child's effects appear on
#      the prime's sender socket
#   d  the child's Warden is killed after its turn is sealed and started
#      again: the proposals survive, the promotion adopts them once
#   e  the child's Warden is killed between its effects and its TurnEnd:
#      the next start withdraws them; none of them is ever promoted
#   s  (S2.4) island-sender in place of the stand-in sender, on a
#      throwaway Island log and broker of its own (127.0.0.1:4790): a
#      promoted child's two effects wait until one is approved and one
#      declined; the approved one is sent once, the declined one never;
#      then a sender that crashes right after its dispatch, a Warden
#      kill and a broker restart: the next pass reports the same dispatch
#      (no second send) and the unapproved effect, unknown now, waits;
#      declined, it is settled and the prime gets its effect authority
#      back
#   f  the prime has a released Irreversible effect and its Warden is
#      killed before a sender reports: after the restart the effect is
#      listed unknown, a new effect is refused (no effect authority), and
#      the stand-in sender's lapse rule (it reports NotApplied) restores
#      authority.  island-sender does not lapse (step 2): run s
#   p  (spike S4, docs/slice-2.md) fork and promote asked for by the agent
#      over the Field, on a prime with "restart": "always" and fork slots:
#      the child learns it is a fork from the island socket's stand-in;
#      a promotion after the prime sealed again is refused (Taso's M7);
#      after a promotion without one the prime is started again on the
#      child's files.  Notes answer S4's questions: what of the prime's files
#      written after the fork Seal survives, what becomes of a released
#      action in a turn that never ended, how long Seals take on a /work
#      of S4_BULK files (FILES:MIB), and which slot a second fork takes.
#      Not in the default ONLY: run it with ONLY=p.
#   m  (spike S5) thinking through island-socket: a prime and its fork
#      child each ask whoami and stream one chat completion from a stub
#      model (scripts/s3/stub-model.py) through the real island-socket on
#      the Vessels' root filesystem; the throwaway log gets one `access`
#      entry per Vessel whose digest matches the agent's own bodies.
#      Notes time the first byte and the whole.  Run it with ONLY=m.
#   v  (the server gate, docs/slice-2.md, "Harness and gates") flare-agent
#      itself in a long-lived Vessel with fork slots, driven by the
#      scripted model (scripts/s4/scripted_model.py) through the real
#      island-socket, with flare-server, island-sender and flare-tui on
#      the throwaway log:
#        v1  a task said in the session is answered there, once
#        v2  the Warden is killed while a task is in work: the next start
#            works it again, and the reply is in the session once
#        v3  a task is forked two ways over the Field: each child posts
#            its notes, the last one done is promoted, the prime is
#            started again on its files, its held answer is the reply and
#            its verse waits; the Warden is killed while it waits; then
#            it is approved and written once, signed as the Vessel
#        v4  the Warden is killed while the children of a fork work: the
#            prime starts again on the fork's Seal, waits for them
#            (--fork-wait), forks none again and answers without them
#        v5  island-socket is killed while the children of a fork work
#            and started again: every child's last note still arrives,
#            and the task is answered once
#      Not in the default ONLY: `just vessel-gate` runs a b c d e f s v.
#
# Each run has its own Vessels under its own root.  The script starts its
# own block log, mounts and coordinator in a work directory, as Taso's
# no-trace gate does, and removes them at the end: it touches nothing in
# /etc/taso or /var/lib/taso, and no firewall rule.
#
# The agent is scripts/s2/s2-agent.py inside the Vessel; the sender is
# scripts/s2/s2-sender.py on the host, a stand-in for island-sender.
#
# Usage (root; needs runc, ublk_drv, cargo, python3, a Taso checkout):
#   TASO=/root/taso scripts/s2-spike.sh
#   ONLY="a f" TASO=/root/taso scripts/s2-spike.sh     some runs only
#   ONLY=p TASO=/root/taso scripts/s2-spike.sh         spike S4
#   ONLY=m TASO=/root/taso scripts/s2-spike.sh         spike S5
#   ONLY=v TASO=/root/taso scripts/s2-spike.sh         the gate's own run
#
# Environment: TASO (/root/taso)  ONLY (a b c d e f s)  LANDLOCK (1)
#              S4_BULK (4000:200: run p's /work, files:MiB)
#              OUT (/root/island-s2-<time>)  KEEP (0: 1 keeps the work
#              directory and its block log)
#
# The verdict is the exit status: 0 only if every check of every run
# passed.  The table is in $OUT/results.tsv, every Warden's output next
# to it.

set -euo pipefail
trap 'echo "STOPPED at line $LINENO (exit $?)" >&2' ERR

TASO=${TASO:-/root/taso}
ONLY=${ONLY:-a b c d e f s}
LANDLOCK=${LANDLOCK:-1}
KEEP=${KEEP:-0}
OUT=${OUT:-/root/island-s2-$(date +%Y%m%d-%H%M%S)}
S2=$(cd "$(dirname "$0")/s2" && pwd)
ISLAND=$(cd "$(dirname "$0")/.." && pwd)
WAIT=1200   # tenths of a second: how long to wait for a Warden or an agent

[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
for t in runc ldd python3 cargo mountpoint; do
    command -v "$t" >/dev/null || { echo "$t is missing" >&2; exit 2; }
done
[ -f "$TASO/Cargo.toml" ] || { echo "no Taso checkout at $TASO (set TASO)" >&2; exit 2; }
for r in $ONLY; do
    case $r in a|b|c|d|e|f|s|p|m|v) ;; *) echo "ONLY: unknown run $r (a b c d e f s p m v; b also does g and h)" >&2; exit 2 ;; esac
done

echo "== building Taso (release) in $TASO at $(git -C "$TASO" log -1 --format='%h %s' 2>/dev/null || echo '?')"
(cd "$TASO" && cargo build --release -q -p taso-warden -p taso-landlock \
    && cargo build --release -q -p taso-blk --bin taso-blkd)
BIN=$TASO/target/release
case " $ONLY " in *" s "*|*" m "*|*" v "*)
    echo "== building island-cli, island-broker, island-sender, island-socket (release) in $ISLAND"
    (cd "$ISLAND" && cargo build --release -q --locked -p island-cli -p island-broker -p island-sender \
        -p island-socket)
    ;;
esac
case " $ONLY " in *" v "*)
    echo "== building flare-agent, flare-server, flare-tui (release) in $ISLAND"
    (cd "$ISLAND" && cargo build --release -q --locked -p flare-agent -p flare-server -p flare-tui)
    ;;
esac
IBIN=$ISLAND/target/release
modprobe ublk_drv
[ -c /dev/ublk-control ] || { echo "/dev/ublk-control missing (ublk_drv)" >&2; exit 2; }

mkdir -p "$OUT"
: > "$OUT/results.tsv"
WORK=$(mktemp -d /var/tmp/island-s2.XXXXXX)
chmod 755 "$WORK"
SOCK=$WORK/coord.sock
BLK_SOCK=/run/island-s2-$$-blk.sock
MNT=/mnt/island-s2-$$
NS="151a4d00000000000000000000000052"
STAMP=$(date +%Y%m%d%H%M%S)

blk() {   # one taso-blkd command; fails unless the answer starts "ok"
    python3 - "$BLK_SOCK" "$@" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(120); s.connect(sys.argv[1])
s.sendall((" ".join(sys.argv[2:]) + "\n").encode())
line = s.makefile().readline().strip()
print(line)
sys.exit(0 if line.startswith("ok") else 1)
PY
}

mounts_under() {   # mount points under $MNT, innermost first
    awk -v root="$MNT" '$5 == root || index($5, root "/") == 1 { print length($5), $5 }' /proc/self/mountinfo \
        | sort -rn | cut -d' ' -f2-
}

kill_runc() {   # containers left by a killed Warden
    [ -d "$WORK/run/runc" ] || return 0
    for c in $(runc --root "$WORK/run/runc" list -q 2>/dev/null); do
        runc --root "$WORK/run/runc" delete --force "$c" 2>/dev/null || true
    done
}

BG_PIDS=()
cleanup() {
    # disown first, so bash does not report each as "Killed".
    for p in "${BG_PIDS[@]}"; do disown "$p" 2>/dev/null || true; kill -KILL "$p" 2>/dev/null || true; done
    [ -n "${ECHO_PID:-}" ] && kill "$ECHO_PID" 2>/dev/null || true
    [ -n "${COORD_PID:-}" ] && kill "$COORD_PID" 2>/dev/null || true
    kill_runc
    while read -r m; do
        [ -n "$m" ] || continue
        fsfreeze -u "$m" 2>/dev/null || true
        umount "$m" 2>/dev/null || umount -l "$m" 2>/dev/null || true
    done < <(mounts_under)
    if [ -n "${BLKD_PID:-}" ] && kill -0 "$BLKD_PID" 2>/dev/null; then
        (blk quit) >/dev/null 2>&1 || kill "$BLKD_PID"
        wait "$BLKD_PID" 2>/dev/null || true
    fi
    left=$(mounts_under | wc -l)
    [ "$left" -eq 0 ] || echo "WARNING: $left mounts left under $MNT"
    rmdir "$MNT" 2>/dev/null || true
    if [ "$KEEP" = 1 ]; then echo "kept: $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

# ── the Vessels' root filesystem ───────────────────────────────────────
ROOTFS=$WORK/rootfs
copy_with_libs() {   # $1 host file -> $2 path in the rootfs, with its libraries
    install -D -m 755 "$1" "$ROOTFS$2"
    local lib
    for lib in $(ldd "$1" 2>/dev/null | grep -o '/[^ ]*' || true); do
        [ -f "$lib" ] && install -D -m 755 "$lib" "$ROOTFS$lib"
    done
}
copy_with_libs "$(command -v python3)" /usr/bin/python3
copy_with_libs "$BIN/taso-landlock" /usr/lib/taso/landlock
PY_LIB=$(python3 -c 'import os; print(os.path.dirname(os.__file__))')
mkdir -p "$ROOTFS$(dirname "$PY_LIB")"
cp -a "$PY_LIB" "$ROOTFS$PY_LIB"
for lib in $(find "$ROOTFS$PY_LIB" -name '*.so' -exec ldd {} + 2>/dev/null | grep -o '/[^ ]*' | sort -u || true); do
    [ -f "$ROOTFS$lib" ] || install -D -m 755 "$lib" "$ROOTFS$lib" 2>/dev/null || true
done
install -D -m 644 "$S2/s2_field.py" "$ROOTFS/usr/lib/s2/s2_field.py"
install -D -m 755 "$S2/s2-agent.py" "$ROOTFS/usr/lib/s2/s2-agent.py"
install -D -m 755 "$S2/s4-agent.py" "$ROOTFS/usr/lib/s2/s4-agent.py"
install -D -m 755 "$S2/s5-agent.py" "$ROOTFS/usr/lib/s2/s5-agent.py"
# The gate's agent is the real one (run v).
[ ! -x "$ISLAND/target/release/flare-agent" ] || copy_with_libs "$ISLAND/target/release/flare-agent" /usr/bin/flare-agent
mkdir -p "$ROOTFS"/{proc,dev,sys,tmp,work,run/taso}
: > "$ROOTFS/run/taso/field.sock"
LLM=/run/island/llm.sock   # where the agent sees the inference socket
python3 "$S2/s2-echo.py" "$ROOTFS$LLM" 2>"$OUT/echo.err" &
ECHO_PID=$!
for _ in $(seq 100); do [ -S "$ROOTFS$LLM" ] && break; sleep 0.05; done
[ -S "$ROOTFS$LLM" ] || { echo "the echo socket did not start; see $OUT/echo.err" >&2; exit 1; }

# ── the block log, the coordinator ─────────────────────────────────────
[ ! -S "$BLK_SOCK" ] || { echo "$BLK_SOCK exists: another run's daemon?" >&2; exit 2; }
mkdir -p "$MNT" "$WORK/spool" "$WORK/bin"
[ -z "$(mounts_under)" ] || { echo "something is already mounted under $MNT" >&2; exit 2; }
cp "$BIN/taso-wardend" "$WORK/bin/"
"$BIN/taso-blkd" "$WORK/blk.log" "$BLK_SOCK" 2>"$OUT/blkd.err" &
BLKD_PID=$!
for _ in $(seq 100); do [ -S "$BLK_SOCK" ] && break; sleep 0.1; done
[ -S "$BLK_SOCK" ] || { echo "taso-blkd did not start; see $OUT/blkd.err" >&2; exit 1; }
"$BIN/taso-coordd" "$WORK/coord.redb" "$SOCK" 2>"$OUT/coordd.err" &
COORD_PID=$!
for _ in $(seq 100); do [ -S "$SOCK" ] && break; sleep 0.05; done
[ -S "$SOCK" ] || { echo "taso-coordd did not start; see $OUT/coordd.err" >&2; exit 1; }
echo "== block log, coordinator and echo socket ready (work $WORK)"

# ── helpers ────────────────────────────────────────────────────────────

# cfg <out> <root> <subject> <slot> <steps> [fork-parent] [promote-child]:
# a Warden config with one Vessel, the node settings as Taso's gate has them
cfg() {
    local a=("$@")
    while [ ${#a[@]} -lt 7 ]; do a+=(""); done
    python3 - "${a[@]}" "$SOCK" "$WORK" "$ROOTFS" "$NS" "$LANDLOCK" "$BLK_SOCK" "$MNT" <<'PY'
import json, os, sys
out, root, subject, slot, steps, parent, promote = sys.argv[1:8]
sock, work, rootfs, ns, landlock, blk_sock, mnt = sys.argv[8:15]
vessel = {"namespace": ns, "subject": subject, "uid_slot": int(slot)}
if parent:
    vessel["fork"] = {"parent": parent, "seal": "last"}   # a fork holds
else:
    vessel["effects"] = "dispatch"                        # the prime (decision D1)
if promote:
    vessel["promote"] = {"child": promote, "seal": "last"}
rt = {"rootfs": rootfs, "host_uid_base": 200000,
      # The first step names this start, so the host never mistakes a report
      # left by an earlier start, or adopted from a promoted child, for its own.
      "args": ["/usr/bin/python3", "/usr/lib/s2/s2-agent.py", "--steps",
               "note:" + os.path.basename(out)[:-len(".json")] + "," + steps],
      "memory_bytes": 268435456, "pids": 64}
if landlock == "1":
    rt["landlock_shim"] = "/usr/lib/taso/landlock"
json.dump({
    "warden": 1,
    "coordinator": {"remote": sock},
    "storage": {"blk": {"socket": blk_sock, "mounts": mnt, "spool": f"{work}/spool"}},
    "root": root, "state_dir": f"{work}/state", "store": f"{work}/store",
    "prefix": "vessels", "run_dir": f"{work}/run", "runtime": rt, "vessels": [vessel],
}, open(out, "w"), indent=1)
PY
}

FAILS=0
check() {   # check <run> <what> <got> <want>
    local verdict=PASS
    [ "$3" = "$4" ] || { verdict=FAIL; FAILS=$((FAILS + 1)); }
    printf '%s\t%s\t%s\tgot=%s\twant=%s\n' "$1" "$verdict" "$2" "$3" "$4" | tee -a "$OUT/results.tsv"
}
note() { printf '%s\tNOTE\t%s\n' "$1" "$2" | tee -a "$OUT/results.tsv"; }

jq_() {   # jq_ <json file> <python expression over d>
    python3 -c 'import json, sys; d = json.load(open(sys.argv[1])); r = eval(sys.argv[2]); print(r if not isinstance(r, (list, dict)) else json.dumps(r))' "$1" "$2"
}

ds_of() {   # the dataset a Warden's stderr names
    grep -o ': dataset [^ ]*$' "$1" 2>/dev/null | head -1 | sed 's/: dataset //' || true
}

start() {   # start <name> <config>: a Warden in the background; sets PID, ERR
    ERR=$OUT/$1.err
    "$WORK/bin/taso-wardend" "$2" >"$OUT/$1.out" 2>"$ERR" &
    PID=$!
    BG_PIDS+=("$PID")
}

run() {   # run <name> <config>: a Warden in the foreground; sets ERR
    ERR=$OUT/$1.err
    if ! "$WORK/bin/taso-wardend" "$2" >"$OUT/$1.out" 2>"$ERR"; then
        echo "Warden $1 failed:" >&2; tail -20 "$ERR" >&2
        return 1
    fi
}

finish() {   # finish <pid>: the Warden must end well
    if ! wait "$1"; then
        echo "Warden ($ERR) failed:" >&2; tail -20 "$ERR" >&2
        return 1
    fi
}

killw() {   # killw <pid>: SIGKILL a Warden, clear its container
    kill -KILL "$1" 2>/dev/null || true
    wait "$1" 2>/dev/null || true
    kill_runc
}

# report_of <err> <events>: wait until the agent's report has at least
# <events> events; copies it next to the Warden's output and prints the
# copy's path.  The report lives in the Vessel's dataset, mounted at
# $MNT/<dataset>, and goes at the next start (it is written after the
# last Seal): the copy is what the checks read.
REPORTS=0
report_of() {
    local err=$1 n=$2 ds path copy start
    start=$(basename "$err" .err)   # the start's name, which its config names too
    for _ in $(seq $WAIT); do
        ds=$(ds_of "$err")
        if [ -n "$ds" ]; then
            path=$MNT/$ds/s2-report.json
            REPORTS=$((REPORTS + 1))
            copy=${err%.err}.report-$REPORTS.json
            # Only this start's report counts (its note), with at least n
            # events besides the note; the copy leaves the note out.
            if [ -f "$path" ] && python3 - "$path" "$copy" "$start" "$n" <<'PY' 2>/dev/null
import json, sys
path, copy, start, n = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
d = json.load(open(path))
notes = [e for e in d["events"] if e["step"] == "note"]
if not notes or notes[0]["text"] != start:
    sys.exit(1)
d["events"] = [e for e in d["events"] if e["step"] != "note"]
if len(d["events"]) < n:
    sys.exit(1)
json.dump(d, open(copy, "w"), indent=1)
PY
            then
                echo "$copy"; return 0
            fi
        fi
        sleep 0.1
    done
    echo "no report with $n event(s) from the agent of $err:" >&2; tail -10 "$err" >&2
    return 1
}

sender_sock() {   # the Warden's sender socket for the Vessel named in <err>
    local ds; ds=$(ds_of "$1")
    echo "$WORK/run/senders/${ds##*/v-}.sock"
}

send() {   # send <err> <out json> [sender options]: the reference sender
    local err=$1 out=$2 s; shift 2
    s=$(sender_sock "$err")
    for _ in $(seq $WAIT); do [ -S "$s" ] && break; sleep 0.1; done
    python3 "$S2/s2-sender.py" --socket "$s" "$@" >"$out" 2>"$out.err"
}

go() { touch "$MNT/$(ds_of "$1")/$2"; }   # go <err> <name>: the host's go-ahead
# forget <err>: before a restart, remove the last start's report from the
# Vessel's dataset, so the next one read is the new start's
forget() { rm -f "$MNT/$(ds_of "$1")/s2-report.json"; }

keys() {   # keys <report> [result]: the declared keys, sorted, comma-joined
    local only=""
    [ -z "${2:-}" ] || only=" and e['result'] == '$2'"
    jq_ "$1" "','.join(sorted(e['key'] for e in d['events'] if e['step'] == 'effect'$only))"
}
open_keys() { jq_ "$1" "','.join(sorted(e['retry'][1] for e in d['$2']))"; }

# prime_seal <run>: the run's prime Vessel seals one turn, no effects; extra steps first
prime_seal() {
    local r=$1 extra=${2:-}
    cfg "$WORK/$r-prime.json" "s2/$STAMP/$r" "$r-prime" 0 "${extra}seal,report"
    run "$r-prime" "$WORK/$r-prime.json"
}

# promote_and_send <run> <child> <tag>: start the prime with "promote", read
# its sender socket and apply; prints the promoted keys
promote_and_send() {
    local r=$1 child=$2 tag=$3
    cfg "$WORK/$r-prime-$tag.json" "s2/$STAMP/$r" "$r-prime" 0 "report,wait:sender-done" "" "$child"
    rm -f "$MNT/$(ds_of "$OUT/$r-prime.err")/sender-done"
    forget "$OUT/$r-prime.err"
    start "$r-prime-$tag" "$WORK/$r-prime-$tag.json"
    report_of "$ERR" 0 >/dev/null   # the agent is up: its report names this start
    send "$ERR" "$OUT/$r-sender-$tag.json" --apply
    go "$ERR" sender-done
    finish "$PID"
    open_keys "$OUT/$r-sender-$tag.json" before
}

# ── a: the prime dispatches ───────────────────────────────────────────
run_a() {
    echo "-- a: the prime dispatches, a sender applies"
    cfg "$WORK/a-prime.json" "s2/$STAMP/a" a-prime 0 \
        "effect:nem,effect:nem,effect:nem,seal,report,wait:sender-done"
    start a-prime "$WORK/a-prime.json"
    local rep; rep=$(report_of "$ERR" 4)
    send "$ERR" "$OUT/a-sender.json" --apply
    go "$ERR" sender-done
    finish "$PID"
    check a "3 effects released" "$(jq_ "$rep" "[e['result'] for e in d['events'] if e['step'] == 'effect']")" \
        '["Released", "Released", "Released"]'
    check a "the sender lists exactly the declared keys" "$(open_keys "$OUT/a-sender.json" before)" "$(keys "$rep")"
    check a "every Applied recorded" "$(jq_ "$OUT/a-sender.json" "[r['refused'] for r in d['reported']]")" \
        '[null, null, null]'
    check a "nothing listed after (none twice)" "$(jq_ "$OUT/a-sender.json" 'len(d["after"])')" 0
}

# ── b, g, h: a child holds; the inference socket ──────────────────────
run_b() {
    echo "-- b, g, h: a fork child holds; the socket in the rootfs"
    prime_seal b "socket:$LLM:1048576,tcp:10.77.0.1:4747,tcp:127.0.0.1:4747,"
    local prep; prep=$(report_of "$OUT/b-prime.err" 1)
    check h "the prime reaches the socket, 1 MiB each way" "$(jq_ "$prep" "d['events'][0]['ok']")" True
    note h "prime: $(jq_ "$prep" "d['events'][0]")"
    check h "the prime has no TCP to 10.77.0.1:4747" "$(jq_ "$prep" "d['events'][1]['reached']")" False
    check h "the prime has no TCP to the host's 127.0.0.1:4747" "$(jq_ "$prep" "d['events'][2]['reached']")" False

    cfg "$WORK/b-child.json" "s2/$STAMP/b" b-child 1 \
        "effect:nem,effect:irr,seal,socket:$LLM:1048576,report,wait:sender-done" b-prime
    start b-child "$WORK/b-child.json"
    local rep; rep=$(report_of "$ERR" 4)
    send "$ERR" "$OUT/b-sender.json"
    go "$ERR" sender-done
    finish "$PID"
    check b "the child's sender socket lists nothing" "$(jq_ "$OUT/b-sender.json" 'len(d["before"])')" 0
    check g "a child's NoExternalMutation effect is held" \
        "$(jq_ "$rep" "[e['result'] for e in d['events'] if e['step'] == 'effect' and e['consequence'] == 'NoExternalMutation']")" \
        '["Held"]'
    check g "a child's Irreversible effect is held" \
        "$(jq_ "$rep" "[e['result'] for e in d['events'] if e['step'] == 'effect' and e['consequence'] == 'Irreversible']")" \
        '["Held"]'
    check h "the fork child reaches the socket, 1 MiB each way" \
        "$(jq_ "$rep" "[e['ok'] for e in d['events'] if e['step'] == 'socket']")" '[true]'
    note h "child: $(jq_ "$rep" "[e for e in d['events'] if e['step'] == 'socket'][0]")"
    note h "echo socket saw: $(tr '\n' ' ' < "$OUT/echo.err")"
}

# ── c: promotion ──────────────────────────────────────────────────────
run_c() {
    echo "-- c: the prime promotes a child"
    prime_seal c
    cfg "$WORK/c-child.json" "s2/$STAMP/c" c-child 1 "effect:nem,effect:nem,seal,report" c-prime
    run c-child "$WORK/c-child.json"
    local rep; rep=$(report_of "$ERR" 3)
    local got; got=$(promote_and_send c c-child 1)
    check c "the prime's sender lists exactly the child's keys" "$got" "$(keys "$rep" Held)"
    check c "nothing listed after the sender applied" "$(jq_ "$OUT/c-sender-1.json" 'len(d["after"])')" 0
}

# ── d: the child's Warden killed after its seal ───────────────────────
run_d() {
    echo "-- d: the child's Warden killed after its turn is sealed"
    prime_seal d
    cfg "$WORK/d-child.json" "s2/$STAMP/d" d-child 1 "effect:nem,effect:nem,seal,report,wait:never" d-prime
    start d-child "$WORK/d-child.json"
    local rep; rep=$(report_of "$ERR" 3)
    cp "$rep" "$OUT/d-child-declared.json"
    killw "$PID"
    forget "$ERR"
    note d "child Warden killed after the seal"
    cfg "$WORK/d-child-again.json" "s2/$STAMP/d" d-child 1 "report" d-prime
    run d-child-again "$WORK/d-child-again.json"
    note d "child restart: $(grep -o 'effects ([A-Za-z]*) reconciled.*' "$ERR" | head -1)"
    local got; got=$(promote_and_send d d-child 1)
    check d "promotion adopts the sealed proposals after the kill" "$got" "$(keys "$OUT/d-child-declared.json" Held)"
    got=$(promote_and_send d d-child 2)
    check d "a second start with the same promotion adopts nothing again" "$got" ""
}

# ── e: a lost turn ────────────────────────────────────────────────────
run_e() {
    echo "-- e: the child's Warden killed before its TurnEnd"
    prime_seal e
    cfg "$WORK/e-child-lost.json" "s2/$STAMP/e" e-child 1 "effect:nem,effect:nem,report,stall" e-prime
    start e-child-lost "$WORK/e-child-lost.json"
    local rep; rep=$(report_of "$ERR" 2)
    cp "$rep" "$OUT/e-lost.json"
    killw "$PID"
    forget "$ERR"
    cfg "$WORK/e-child.json" "s2/$STAMP/e" e-child 1 "effect:nem,effect:nem,seal,report" e-prime
    run e-child "$WORK/e-child.json"
    local withdrawn=no
    grep -q 'withdrawn: \[OpId' "$ERR" && withdrawn=yes
    check e "the next start withdraws the lost turn's proposals" "$withdrawn" yes
    note e "child restart: $(grep -o 'effects ([A-Za-z]*) reconciled.*' "$ERR" | head -1)"
    rep=$(report_of "$ERR" 3)
    local got; got=$(promote_and_send e e-child 1)
    check e "the promotion carries the second run's keys" "$got" "$(keys "$rep" Held)"
    local lost; lost=$(keys "$OUT/e-lost.json")
    local leaked; leaked=$(comm -12 <(tr ',' '\n' <<<"$lost" | sort) <(tr ',' '\n' <<<"$got" | sort) | grep -c . || true)
    check e "none of the lost turn's keys is ever promoted" "$leaked" 0
}

# ── f: a barrier lost in a Warden kill, and the lapse rule ─────────────
run_f() {
    echo "-- f: a released Irreversible effect, the Warden killed, the lapse rule"
    cfg "$WORK/f-prime-1.json" "s2/$STAMP/f" f-prime 0 "effect:irr,seal,report,wait:never"
    start f-prime-1 "$WORK/f-prime-1.json"
    local rep; rep=$(report_of "$ERR" 2)
    cp "$rep" "$OUT/f-first.json"
    check f "the Irreversible effect is released" "$(jq_ "$rep" "d['events'][0]['result']")" Released
    killw "$PID"
    forget "$ERR"
    note f "Warden killed with the effect released and no outcome reported"

    cfg "$WORK/f-prime-2.json" "s2/$STAMP/f" f-prime 0 \
        "effect:nem,report,wait:lapsed,effect:nem,seal,report,wait:finished"
    start f-prime-2 "$WORK/f-prime-2.json"
    rep=$(report_of "$ERR" 1)
    note f "restart: $(grep -o 'effects ([A-Za-z]*) reconciled.*' "$ERR" | head -1)"
    check f "a new effect is refused: no effect authority" \
        "$(jq_ "$rep" "d['events'][0]['result'] + ': ' + d['events'][0].get('reason', '')[:19]")" \
        "Refused: no effect authority"
    send "$ERR" "$OUT/f-sender-lapse.json" --lapse-unknown
    check f "the sender lists the lost effect as unknown" \
        "$(jq_ "$OUT/f-sender-lapse.json" "[(e['consequence'], e['unknown']) for e in d['before']]")" \
        '[["Irreversible", true]]'
    check f "the same effect, by its key" "$(open_keys "$OUT/f-sender-lapse.json" before)" "$(keys "$OUT/f-first.json")"
    check f "NotApplied (lapsed) is recorded" "$(jq_ "$OUT/f-sender-lapse.json" "[r['refused'] for r in d['reported']]")" '[null]'
    go "$ERR" lapsed
    rep=$(report_of "$ERR" 4)
    check f "after the lapse a new effect is released" \
        "$(jq_ "$rep" "[e['result'] for e in d['events'] if e['step'] == 'effect']")" '["Refused", "Released"]'
    send "$ERR" "$OUT/f-sender-apply.json" --apply
    check f "the sender lists only the new effect" "$(open_keys "$OUT/f-sender-apply.json" before)" "$(keys "$rep" Released)"
    go "$ERR" finished
    finish "$PID"
}


# ── s: island-sender (S2.4), on a throwaway Island log and broker ──────
ISL_PORT=4790
island_up() {   # the broker on the throwaway log
    "$IBIN/island-broker" --log "$ISL/island.log" --appender-key "$ISL/appender.key" \
        --root "$ISL_ROOT" --listen "127.0.0.1:$ISL_PORT" >>"$OUT/island-broker.out" 2>&1 &
    ISL_BROKER=$!
    BG_PIDS+=("$ISL_BROKER")
    for _ in $(seq 100); do (exec 3<>"/dev/tcp/127.0.0.1/$ISL_PORT") 2>/dev/null && return 0; sleep 0.1; done
    echo "the throwaway broker did not start; see $OUT/island-broker.out" >&2; return 1
}
island_down() { kill "$ISL_BROKER" 2>/dev/null || true; wait "$ISL_BROKER" 2>/dev/null || true; }
island_setup() {
    if [ -n "${ISL_ROOT:-}" ]; then   # once, for every run that uses it
        # A run before this one may have left its broker down (run s does).
        (exec 3<>"/dev/tcp/127.0.0.1/$ISL_PORT") 2>/dev/null || island_up
        return 0
    fi
    ISL=$WORK/island
    mkdir -p "$ISL"
    local who scope
    for who in root appender approver sender socket; do "$IBIN/island-cli" keygen "$ISL/$who.key" >/dev/null; done
    ISL_ROOT=$("$IBIN/island-cli" public "$ISL/root.key")
    "$IBIN/island-cli" init --log "$ISL/island.log" --appender-key "$ISL/appender.key" \
        --root-key "$ISL/root.key" >"$OUT/island-init.out"
    local right
    for who in approver:approvals:approval sender:approvals:dispatch socket:access:access; do
        IFS=: read -r who scope right <<<"$who"
        "$IBIN/island-cli" grant --log "$ISL/island.log" --appender-key "$ISL/appender.key" \
            --root-key "$ISL/root.key" --holder "$("$IBIN/island-cli" public "$ISL/$who.key")" \
            --scope "$scope" --rights "$right" >>"$OUT/island-init.out"
    done
    island_up
    note s "throwaway Island log $ISL/island.log, broker 127.0.0.1:$ISL_PORT"
}

# isend <err> <out> [options]: one island-sender pass; sets ISEND_CODE
isend() {
    local err=$1 out=$2 s; shift 2
    s=$(sender_sock "$err")
    ISEND_CODE=0
    "$IBIN/island-sender" --socket "$s" --broker "http://127.0.0.1:$ISL_PORT" --root "$ISL_ROOT" \
        --key "$ISL/sender.key" --once --wait 120 "$@" \
        >"$out" 2>"$out.err" || ISEND_CODE=$?
}
lines() { python3 -c 'import json, sys; print(json.dumps([json.loads(l) for l in open(sys.argv[1]) if l.strip()]))' "$1" > "$1.json"; echo "$1.json"; }
iapprove() {   # iapprove <hash> [--decline]
    "$IBIN/island-cli" approve --broker "http://127.0.0.1:$ISL_PORT" --root "$ISL_ROOT" \
        --key "$ISL/approver.key" "$@" >>"$OUT/island-approve.out"
}
sent_marks() {   # sent_marks <vessel-id>: how many dispatch entries name this Vessel
    "$IBIN/island-cli" tail --broker "http://127.0.0.1:$ISL_PORT" --root "$ISL_ROOT" -n 500 \
        | grep -c " dispatch .* vessel $1 op " || true
}

run_s() {
    echo "-- s: island-sender: approvals, a crash after a send, an unknown action waits"
    island_setup

    # s1: a promoted child's two effects; one approved, one declined
    prime_seal s1
    cfg "$WORK/s1-child.json" "s2/$STAMP/s1" s1-child 1 "effect:irr,effect:irr,seal,report" s1-prime
    run s1-child "$WORK/s1-child.json"
    cfg "$WORK/s1-prime-p.json" "s2/$STAMP/s1" s1-prime 0 "report,wait:sender-done" "" s1-child
    forget "$OUT/s1-prime.err"
    start s1-prime-p "$WORK/s1-prime-p.json"
    report_of "$ERR" 0 >/dev/null   # the agent is up: its report names this start
    local vessel; vessel=$(basename "$(sender_sock "$ERR")" .sock)
    isend "$ERR" "$OUT/s1-pass1.jsonl"
    local p1; p1=$(lines "$OUT/s1-pass1.jsonl")
    check s "nothing approved: both promoted effects wait" "$(jq_ "$p1" "[x['action'] for x in d]")" '["wait", "wait"]'
    iapprove "$(jq_ "$p1" "d[0]['request_hash']")"
    iapprove "$(jq_ "$p1" "d[1]['request_hash']")" --decline
    isend "$ERR" "$OUT/s1-pass2.jsonl"
    local p2; p2=$(lines "$OUT/s1-pass2.jsonl")
    check s "the approved one is sent, the declined one is not" \
        "$(jq_ "$p2" "[(x['action'], x.get('reported')) for x in d]")" '[["send", true], ["decline", true]]'
    isend "$ERR" "$OUT/s1-pass3.jsonl"
    check s "nothing is left to send" "$(grep -c . "$OUT/s1-pass3.jsonl" || true)" 0
    go "$ERR" sender-done
    finish "$PID"
    check s "the log has one dispatch for this Vessel" "$(sent_marks "$vessel")" 1

    # s2: crash after the dispatch, then a Warden kill and a broker restart
    cfg "$WORK/s2-prime-1.json" "s2/$STAMP/s2" s2-prime 0 "effect:irr,effect:irr,seal,report,wait:never"
    start s2-prime-1 "$WORK/s2-prime-1.json"
    report_of "$ERR" 3 >/dev/null
    vessel=$(basename "$(sender_sock "$ERR")" .sock)
    isend "$ERR" "$OUT/s2-pass1.jsonl"
    p1=$(lines "$OUT/s2-pass1.jsonl")
    iapprove "$(jq_ "$p1" "d[0]['request_hash']")"
    isend "$ERR" "$OUT/s2-crash.jsonl" --stop-after-send
    local crash; crash=$(lines "$OUT/s2-crash.jsonl")
    check s "the sender stops after its dispatch, before reporting" \
        "$ISEND_CODE $(jq_ "$crash" "[(x['action'], x.get('stopped_before_report')) for x in d]")" '3 [["send", true]]'
    killw "$PID"
    forget "$ERR"
    island_down
    island_up
    note s "Warden killed and broker restarted after the dispatch"
    cfg "$WORK/s2-prime-2.json" "s2/$STAMP/s2" s2-prime 0 \
        "effect:nem,report,wait:resumed,effect:nem,seal,report,wait:finished"
    start s2-prime-2 "$WORK/s2-prime-2.json"
    local rep; rep=$(report_of "$ERR" 1)
    check s "after the kill a new effect is refused: no effect authority" \
        "$(jq_ "$rep" "d['events'][0]['result'] + ': ' + d['events'][0].get('reason', '')[:19]")" \
        "Refused: no effect authority"
    isend "$ERR" "$OUT/s2-pass2.jsonl"
    p2=$(lines "$OUT/s2-pass2.jsonl")
    check s "the sent one is reported from its dispatch, the unapproved one waits" \
        "$(jq_ "$p2" "[(x['action'], x['unknown'], x.get('reported')) for x in d]")" \
        '[["already-sent", true, true], ["wait", true, null]]'
    check s "the same dispatch as before the crash: no second send" \
        "$(jq_ "$p2" "d[0].get('sent_at')")" "$(jq_ "$crash" "d[0]['sent_at']")"
    # Waiting is not lapsing: until it is answered the Vessel releases
    # nothing new. Declined, it is settled and authority comes back.
    iapprove "$(jq_ "$p2" "d[1]['request_hash']")" --decline
    isend "$ERR" "$OUT/s2-pass2b.jsonl"
    check s "declined after the restart, it is reported not applied" \
        "$(jq_ "$(lines "$OUT/s2-pass2b.jsonl")" "[(x['action'], x.get('reported')) for x in d]")" '[["decline", true]]'
    go "$ERR" resumed
    rep=$(report_of "$ERR" 4)
    check s "effect authority is back: the next effect is released" \
        "$(jq_ "$rep" "[e['result'] for e in d['events'] if e['step'] == 'effect']")" '["Refused", "Released"]'
    isend "$ERR" "$OUT/s2-pass3.jsonl"
    p2=$(lines "$OUT/s2-pass3.jsonl")
    check s "the new, unapproved effect waits" "$(jq_ "$p2" "[x['action'] for x in d]")" '["wait"]'
    go "$ERR" finished
    finish "$PID"
    check s "still one dispatch for this Vessel" "$(sent_marks "$vessel")" 1
    island_down
}

# ── p: spike S4, fork and promote over the Field ───────────────────────
S4_BULK=${S4_BULK:-4000:200}
# Slots 20 to 23: the same shape as flare-home's entry (prime 10, forks
# from 11), on uid ranges no Vessel of the node's own config uses.
S4_PRIME=20
S4_FORKS="21 22 23"

# cfg4 <out> <root> <subject> <prime steps> <fork steps>: a long-lived
# prime with fork slots, as flare-home's entry will be
cfg4() {
    python3 - "$@" "$SOCK" "$WORK" "$ROOTFS" "$NS" "$LANDLOCK" "$BLK_SOCK" "$MNT" "$S4_PRIME" "$S4_FORKS" <<'PY'
import json, sys
out, root, subject, prime, fork = sys.argv[1:6]
sock, work, rootfs, ns, landlock, blk_sock, mnt, slot, forks = sys.argv[6:15]
rt = {"rootfs": rootfs, "host_uid_base": 200000,
      "args": ["/usr/bin/python3", "/usr/lib/s2/s4-agent.py", "--prime", prime, "--fork", fork],
      "memory_bytes": 1073741824, "pids": 64}
if landlock == "1":
    rt["landlock_shim"] = "/usr/lib/taso/landlock"
vessel = {"namespace": ns, "subject": subject, "uid_slot": int(slot), "effects": "dispatch",
          "restart": "always", "forks": {"slots": [int(s) for s in forks.split()]}}
json.dump({
    "warden": 1,
    "coordinator": {"remote": sock},
    "storage": {"blk": {"socket": blk_sock, "mounts": mnt, "spool": f"{work}/spool"}},
    "root": root, "state_dir": f"{work}/state", "store": f"{work}/store",
    "prefix": "vessels", "run_dir": f"{work}/run", "runtime": rt, "vessels": [vessel],
}, open(out, "w"), indent=1)
PY
}

# jwait <journal> <predicate over e> <seconds>: the first journal event that
# holds, printed as JSON; fails after <seconds>
jwait() {
    python3 - "$@" <<'PY'
import json, sys, time
path, pred, secs = sys.argv[1], sys.argv[2], float(sys.argv[3])
end = time.time() + secs
while time.time() < end:
    try:
        with open(path) as fh:
            for line in fh:
                e = json.loads(line)
                if eval(pred):
                    print(json.dumps(e))
                    sys.exit(0)
    except FileNotFoundError:
        pass
    time.sleep(0.2)
sys.exit(1)
PY
}

# jx <journal> <expression over d, the events>: its value (JSON for lists)
jx() {
    python3 - "$@" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    d = [json.loads(line) for line in fh]
P = [e for e in d if e["slot"] == 20]
r = eval(sys.argv[2])
print(json.dumps(r) if isinstance(r, (list, dict, bool)) or r is None else r)
PY
}

run_p() {
    echo "-- p: spike S4, fork and promote over the Field, restart always"
    local j=$OUT/p-journal.jsonl island=$ROOTFS/run/island/island.sock
    python3 "$S2/s4-host.py" --socket "$island" --journal "$j" --prime-slot "$S4_PRIME" \
        2>"$OUT/s4-host.err" &
    BG_PIDS+=("$!")
    for _ in $(seq 100); do [ -S "$island" ] && break; sleep 0.05; done
    [ -S "$island" ] || { echo "s4-host did not start; see $OUT/s4-host.err" >&2; return 1; }

    # Taso's rule M7, found by the first run of p: a prime that seals
    # again after a fork can no longer promote that child.  So the plan
    # forks a first child and seals once more (that promotion must be
    # refused), then forks a second from the newest Seal and promotes it
    # without sealing in between.
    local prime fork
    prime="mark:prime-start,bulk:$S4_BULK,seal,touch:40,seal,mark:fork-task,seal"     # 0-6
    prime+=",fork,mark:prime-after-fork,seal,waitnote:1,promote"                    # 7-11
    prime+=",fork,waitnote:2,mark:prime-unsealed,effect:irr:unended,promote"         # 12-16
    prime+=",ls,result:unended,effect:irr:after,fork,waitnote:3:120,idle"           # 17-22
    fork="ls,rm:fork-task,mark:child-made,effect:irr:child-a,setprime:17,seal,note:done,exit"
    cfg4 "$WORK/p.json" "s4/$STAMP/p" p-prime "$prime" "$fork"
    start p "$WORK/p.json"
    local w=$PID
    if ! jwait "$j" 'e["slot"] == 20 and e["step"] == "idle"' 1800 >/dev/null; then
        check p "the prime reaches the end of its plan" "no" "yes"
        note p "journal tail: $(tail -n 5 "$j" 2>/dev/null | tr '\n' ' ')"
        note p "Warden: $(tail -n 5 "$ERR" | tr '\n' ' ')"
        killw "$w"
        return 0
    fi
    jwait "$j" 'e["slot"] == 23 and e["step"] == "exit"' 120 >/dev/null || true
    local s; s=$(sender_sock "$ERR")
    python3 "$S2/s2-sender.py" --socket "$s" >"$OUT/p-open.json" 2>"$OUT/p-open.err" || true
    killw "$w"

    check p "the prime forks over the Field, three times, one slot each" \
        "$(jx "$j" '[e["result"] for e in P if e["step"] == "fork"]')" '["Forked", "Forked", "Forked"]'
    note p "forks: $(jx "$j" '[(e["child"], e["seal"], e["refusals"], e["seconds"]) for e in P if e["step"] == "fork"]')"
    check p "the child learns from whoami that it is a fork, in slot 21" \
        "$(jx "$j" '[e["role"] for e in d if e["slot"] == 21 and e["step"] == "start"][:1]')" '["fork"]'
    check p "the child starts on the fork Seal: the task there, later prime files not" \
        "$(jx "$j" '[(("fork-task" in e["files"]), ("prime-after-fork" in e["files"])) for e in d if e["slot"] == 21 and e["step"] == "start"][:1]')" \
        '[[true, false]]'
    check p "the child's effect is held" \
        "$(jx "$j" '[e["result"] for e in d if e["slot"] == 21 and e["step"] == "effect"]')" '["Held"]'
    check p "S4 Q2 (M7): a promotion after the prime sealed again is refused" \
        "$(jx "$j" '[("sealed again" in (e.get("reason") or "")) for e in P if e["step"] == "promote"][:1]')" '[true]'
    # The run may end before the agent journals the second reply: the
    # restart is the proof; this is what the agent saw, if it saw it.
    note p "promote replies: $(jx "$j" '[(e.get("child"), e["result"], e.get("seal"), e.get("adopted"), e.get("reason")) for e in P if e["step"] == "promote"]')"
    check p "the prime is started again after the second promotion, at the step its child left it" \
        "$(jx "$j" '[e["resume_at"] for e in P if e["step"] == "start"]')" '[0, 17]'
    check p "S4 Q1: it starts on the child's files (child-made there, fork-task gone)" \
        "$(jx "$j" '[("child-made" in e["files"], "fork-task" in e["files"]) for e in P if e["step"] == "ls"]')" \
        '[[true, false]]'
    check p "S4 Q2: the prime's unsealed files are gone after the promotion" \
        "$(jx "$j" '[("prime-unsealed" in e["files"]) for e in P if e["step"] == "ls"]')" '[false]'
    note p "S4 Q2: Hello after the promotion, last sealed turn: $(jx "$j" '[e["last_sealed"] for e in P if e["step"] == "start"]')"
    note p "S4 Q3: the unended turn's released action: declared $(jx "$j" '[(e["result"], e["op"]) for e in P if e["step"] == "effect" and e["tag"] == "unended"]'), after the restart $(jx "$j" '[(e["state"], e.get("reason")) for e in P if e["step"] == "result"]')"
    note p "S4 Q3: a new Irreversible effect after the restart: $(jx "$j" '[(e["result"], e.get("reason")) for e in P if e["step"] == "effect" and e["tag"] == "after"]')"
    local keys; keys=$(python3 -c 'import hashlib; print(" ".join(t + "=" + hashlib.sha256(("s4:" + t).encode()).digest()[:16].hex() for t in ("unended", "child-a", "after")))')
    note p "S4 Q3: the prime's sender socket lists (op, key, unknown): $(jq_ "$OUT/p-open.json" "[(e['op'], e['retry'][1], e['unknown']) for e in d['before']]") with keys $keys"
    check p "the adopted child effect is on the prime's sender socket" \
        "$(jq_ "$OUT/p-open.json" "'$(cut -d' ' -f2 <<<"$keys" | cut -d= -f2)' in [e['retry'][1] for e in d['before']]")" True
    note p "S4 Q4: bulk write $(jx "$j" '[(e["files"], e["bytes"], e["seconds"]) for e in P if e["step"] == "bulk"]'); seals (s): $(jx "$j" '[e["seconds"] for e in P if e["step"] == "seal"]') (bulk, touch, fork task, after fork)"
    note p "S4 Q4: the second promote's declaration to the next start: $(jx "$j" 'round([e["t"] for e in P if e["step"] == "start"][-1] - [e["t"] for e in P if e["step"] == "effect" and e["tag"] == "unended"][0], 2)') s"
    check p "S4 Q5: each fork takes the next slot, never one again" \
        "$(jx "$j" '[e["slot"] for e in d if e["step"] == "start" and e["role"] == "fork"]')" '[21, 22, 23]'
    note p "S4 Q5: after the run, datasets mounted under $MNT: $(mounts_under | wc -l); state entries: $(ls "$WORK/state" 2>/dev/null | tr '\n' ' ')"
}

# ── m: spike S5, thinking through island-socket ──────────────────────
S5_PRIME=30

# m_config <out>: the Warden config of run m: a prime with one fork slot
m_config() {
    python3 - "$1" "$SOCK" "$WORK" "$ROOTFS" "$NS" "$LANDLOCK" "$BLK_SOCK" "$MNT" "$S5_PRIME" <<'PY'
import json, sys
out, sock, work, rootfs, ns, landlock, blk_sock, mnt, slot = sys.argv[1:10]
rt = {"rootfs": rootfs, "host_uid_base": 200000,
      "args": ["/usr/bin/python3", "/usr/lib/s2/s5-agent.py"], "memory_bytes": 1073741824, "pids": 64}
if landlock == "1":
    rt["landlock_shim"] = "/usr/lib/taso/landlock"
json.dump({
    "warden": 1, "coordinator": {"remote": sock},
    "storage": {"blk": {"socket": blk_sock, "mounts": mnt, "spool": f"{work}/spool"}},
    "root": "s5/m", "state_dir": f"{work}/state", "store": f"{work}/store",
    "prefix": "vessels", "run_dir": f"{work}/run", "runtime": rt,
    "vessels": [{"namespace": ns, "subject": "m-prime", "uid_slot": int(slot), "effects": "hold",
                 "forks": {"slots": [int(slot) + 1]}}],
}, open(out, "w"), indent=1)
PY
}

# m_socket_config <out> <stub port> <state dir>: island-socket's config
m_socket_config() {
    printf '%s\n' \
        "socket = \"$ROOTFS/run/island/island.sock\"" \
        "warden_config = \"$WORK/m.json\"" \
        "state_dir = \"$3\"" \
        "broker = \"http://127.0.0.1:$ISL_PORT\"" \
        "root = \"$ISL_ROOT\"" \
        "key = \"$ISL/socket.key\"" \
        "access_every_secs = 2" \
        "default = \"stub\"" \
        "[provider.stub]" \
        "base_url = \"http://127.0.0.1:$2/v1\"" \
        "models = [\"stub-model\"]" \
        "budget = { calls = 100, bytes = 100000000 }" >"$1"
}

# m_role <report>: "prime" or "fork" once that agent has thought (and the
# prime has forked), else nothing
m_role() {
    jq_ "$1" "(d['whoami']['role'] if 'think' in d and (d['whoami']['role'] == 'fork' or 'fork' in d) else '')" 2>/dev/null || true
}

run_m() {
    echo "-- m: spike S5, a prime and its child think through island-socket"
    island_setup
    local j=$OUT/m isock=$ROOTFS/run/island/island.sock port isocket_pid w ds prime="" child="" who dir vessel tail
    mkdir -p "$j"
    port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
    python3 "$ISLAND/scripts/s3/stub-model.py" --listen "127.0.0.1:$port" --bytes 400000 --pieces 800 \
        --delay 0.002 --requests "$j/stub-requests.jsonl" 2>"$j/stub.err" &
    BG_PIDS+=("$!")
    # The Warden config first: island-socket reads its callers from it.
    m_config "$WORK/m.json"
    m_socket_config "$j/socket.toml" "$port" "$j/state"
    "$IBIN/island-socket" serve --config "$j/socket.toml" 2>"$j/socket.err" &
    isocket_pid=$!
    BG_PIDS+=("$isocket_pid")
    for _ in $(seq 100); do [ -S "$isock" ] && break; sleep 0.05; done
    [ -S "$isock" ] || { echo "island-socket did not start; see $j/socket.err" >&2; return 1; }

    start m "$WORK/m.json"
    w=$PID
    # Two reports: the prime's (it thought and forked) and the child's.
    for _ in $(seq 1200); do
        for ds in $(grep -o ': dataset [^ ]*$' "$ERR" 2>/dev/null | sed 's/: dataset //' | sort -u); do
            [ -f "$MNT/$ds/s5-report.json" ] || continue
            case $(m_role "$MNT/$ds/s5-report.json") in
                prime) prime=$MNT/$ds ;;
                fork) child=$MNT/$ds ;;
            esac
        done
        [ -n "$prime" ] && [ -n "$child" ] && break
        sleep 0.1
    done
    sleep 3   # one access flush and a half
    killw "$w"
    kill "$isocket_pid" 2>/dev/null || true
    wait "$isocket_pid" 2>/dev/null || true
    if [ -z "$prime" ] || [ -z "$child" ]; then
        check m "both the prime and its child report" "prime=${prime:-none} child=${child:-none}" both
        note m "Warden: $(tail -n 5 "$ERR" | tr '\n' ' ')"
        note m "island-socket: $(tail -n 5 "$j/socket.err" | tr '\n' ' ')"
        return 0
    fi
    for who in prime child; do
        if [ "$who" = prime ]; then dir=$prime; else dir=$child; fi
        cp "$dir/s5-report.json" "$j/$who.json"
        cp "$dir/s5-request.json" "$j/$who-request.json"
        cp "$dir/s5-response.bin" "$j/$who-response.bin"
        "$IBIN/island-socket" call-digest --request "$j/$who-request.json" \
            --response "$j/$who-response.bin" >"$j/$who-call"
        "$IBIN/island-socket" batch-digest "$(cat "$j/$who-call")" >"$j/$who-batch"
    done
    check m "whoami: the prime, in its slot, with its one fork left" \
        "$(jq_ "$j/prime.json" "[d['whoami'][k] for k in ('status', 'role', 'slot', 'forks_left')]")" \
        "[200, \"prime\", $S5_PRIME, 1]"
    check m "whoami: the child learns it is the fork, in the next slot" \
        "$(jq_ "$j/child.json" "[d['whoami'][k] for k in ('status', 'role', 'slot', 'prime')]")" \
        "[200, \"fork\", $((S5_PRIME + 1)), \"m-prime\"]"
    check m "whoami: the prime's id is the one the Warden names its dataset with" \
        "$(jq_ "$j/prime.json" "d['whoami']['vessel']")" "${prime##*/v-}"
    for who in prime child; do
        check m "$who: the streamed answer arrives whole, from the stub provider" \
            "$(jq_ "$j/$who.json" "[d['think'][k] for k in ('status', 'provider', 'done')]")" '[200, "stub", true]'
        check m "$who: it arrives in pieces, as it is made" \
            "$(jq_ "$j/$who.json" "d['think']['reads'] > 20 and d['think']['events'] > 700")" True
        note m "$who: first byte $(jq_ "$j/$who.json" "d['think']['first_byte_s']") s, whole $(jq_ "$j/$who.json" "d['think']['total_s']") s, $(jq_ "$j/$who.json" "d['think']['response_bytes']") bytes in $(jq_ "$j/$who.json" "d['think']['reads']") reads; request $(jq_ "$j/$who.json" "d['think']['request_bytes']") bytes"
    done
    tail=$("$IBIN/island-cli" tail --broker "http://127.0.0.1:$ISL_PORT" --root "$ISL_ROOT" -n 50)
    for who in prime child; do
        vessel=$(jq_ "$j/$who.json" "d['whoami']['vessel']")
        check m "$who: one access entry, its bytes and digest the agent's own" \
            "$(grep -c " access .*vessel $vessel Inference via \"stub\", 1 call(s), $(jq_ "$j/$who.json" "d['think']['request_bytes']") bytes in, $(jq_ "$j/$who.json" "d['think']['response_bytes']") out, digest $(cut -c1-8 "$j/$who-batch")" <<<"$tail" || true)" 1
    done
    check m "the provider was sent no key" \
        "$(python3 -c 'import json, sys; print(sorted({r["auth"] for r in map(json.loads, open(sys.argv[1]))}, key=str))' "$j/stub-requests.jsonl")" \
        "[None]"
}

# ── v: the server gate, flare-agent with the scripted model ───────────
V_PRIME=40
V_FORKS="41 42 43 44 45 46"
V_FORK_WAIT=45   # seconds the prime waits for children that never finish

# v_config <out>: flare-home's shape (docs/slice-2.md, "The Vessel and the
# agent"): a long-lived dispatching prime with fork slots, running flare-agent
v_config() {
    python3 - "$1" "$SOCK" "$WORK" "$ROOTFS" "$NS" "$LANDLOCK" "$BLK_SOCK" "$MNT" "$V_PRIME" "$V_FORKS" "$V_FORK_WAIT" <<'PY'
import json, sys
out, sock, work, rootfs, ns, landlock, blk_sock, mnt, slot, forks, wait = sys.argv[1:12]
rt = {"rootfs": rootfs, "host_uid_base": 200000,
      "args": ["/usr/bin/flare-agent", "--fork-wait", wait],
      "memory_bytes": 1073741824, "pids": 128}
if landlock == "1":
    rt["landlock_shim"] = "/usr/lib/taso/landlock"
json.dump({
    "warden": 1, "coordinator": {"remote": sock},
    "storage": {"blk": {"socket": blk_sock, "mounts": mnt, "spool": f"{work}/spool"}},
    "root": "gate/v", "state_dir": f"{work}/state", "store": f"{work}/store",
    "prefix": "vessels", "run_dir": f"{work}/run", "runtime": rt,
    "vessels": [{"namespace": ns, "subject": "v-prime", "uid_slot": int(slot), "effects": "dispatch",
                 "restart": "always", "forks": {"slots": [int(s) for s in forks.split()]}}],
}, open(out, "w"), indent=1)
PY
}

# sx <expression>: over the session main's events E; R the agent's replies
# (payloads), N the fork notes, W the actions waiting and A those settled
# (events), U the messages said (events)
sx() {
    python3 - "$V/flare/sessions/main.jsonl" "$1" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as fh:
        E = [json.loads(line) for line in fh if line.strip()]
except FileNotFoundError:
    E = []
R = [e["payload"] for e in E if e["kind"] == "agent_text"]
N = [e["payload"] for e in E if e["kind"] == "fork_note"]
W = [e for e in E if e["kind"] == "action_waiting"]
A = [e for e in E if e["kind"] == "action_settled"]
U = [e for e in E if e["kind"] == "user_message"]
r = eval(sys.argv[2])
print(json.dumps(r) if isinstance(r, (list, dict, bool)) or r is None else r)
PY
}
swait() {   # swait <seconds> <expression>: until sx says true
    local secs=$1
    for _ in $(seq $((secs * 5))); do [ "$(sx "$2" 2>/dev/null)" = true ] && return 0; sleep 0.2; done
    return 1
}

vdir() { echo "$MNT/$(ds_of "${ERR:-/dev/null}")"; }   # the prime's dataset, mounted

# vlog <expression>: over the events d of flare-agent's log in the prime's
# dataset (.flare/log.jsonl: what its last Seal held, and this start)
vlog() {
    python3 - "$(vdir)/.flare/log.jsonl" "$1" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as fh:
        d = [json.loads(line) for line in fh if line.strip()]
except FileNotFoundError:
    d = []
r = eval(sys.argv[2])
print(json.dumps(r) if isinstance(r, (list, dict, bool)) or r is None else r)
PY
}
vlog_wait() {   # vlog_wait <seconds> <expression>
    local secs=$1
    for _ in $(seq $((secs * 5))); do [ "$(vlog "$2" 2>/dev/null)" = true ] && return 0; sleep 0.2; done
    return 1
}

# vsay <client> <text> [seconds]: say it in the session with flare-tui and
# wait for the agent's reply; prints what the terminal showed
vsay() {
    printf '%s\n' "$2" | timeout $(( ${3:-120} + 30 )) "$IBIN/flare-tui" \
        --server "ws://127.0.0.1:$V_SESSION/session" --session main --client "$1" --after 999999999 \
        --root "$ISL_ROOT" --broker "http://127.0.0.1:$ISL_PORT" --replica "$V/replica-$1.log" \
        --reply-wait "${3:-120}" 2>&1 || true
}
vtask() { sx "([e['seq'] for e in U if e['payload']['client'] == '$1'] or [-1])[-1]"; }   # the task a client's message became

v_whoami() {   # what the island socket tells the prime about itself, as its uid
    python3 - "$ROOTFS/run/island/island.sock" "$((200000 + V_PRIME * 65536))" "$1" <<'PY' 2>/dev/null || echo no-answer
import http.client, json, os, socket, sys
os.setgid(int(sys.argv[2])); os.setuid(int(sys.argv[2]))
c = http.client.HTTPConnection("island")
c.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
c.sock.connect(sys.argv[1])
c.request("GET", "/whoami")
print(json.loads(c.getresponse().read())[sys.argv[3]])
PY
}

# v_verses <public key> [text]: the verses on `flare` that key wrote, with
# that text if one is given.  island-cli pads its columns, so the fields
# are matched, not the line.
v_verses() {
    "$IBIN/island-cli" tail --broker "http://127.0.0.1:$ISL_PORT" --root "$ISL_ROOT" -n 500 \
        | awk -v w="${1:0:8}" '$3 == "verse" && $4 == "flare" && $5 == w' | grep -c -F -- "${2:-}" || true
}

v_socket_up() {
    rm -f "$ROOTFS/run/island/island.sock"   # a killed one's, or another run's
    "$IBIN/island-socket" serve --config "$V/socket.toml" 2>>"$V/socket.err" &
    V_SOCKET=$!
    BG_PIDS+=("$V_SOCKET")
    for _ in $(seq 100); do [ -S "$ROOTFS/run/island/island.sock" ] && return 0; sleep 0.05; done
    echo "island-socket did not start; see $V/socket.err" >&2; return 1
}

v_warden() {   # v_warden <name>: start the Warden and wait for the prime's agent to start under it
    # Its starts are told apart by their ids: the log shows the last Seal's
    # history, so a start that was never sealed is gone from it.
    local last
    last=$(vlog '[e["start"] for e in d if e["event"] == "start" and "fork" not in e][-1:]')
    start "$1" "$WORK/v.json"
    V_WARDEN=$PID
    vlog_wait 120 "[e['start'] for e in d if e['event'] == 'start' and 'fork' not in e][-1:] not in ([], $last)" \
        || { echo "flare-agent did not start under Warden $1:" >&2; tail -5 "$ERR" >&2; return 1; }
}

run_v() {
    echo "-- v: the server gate: flare-agent in a Vessel, a task, a fork, a promotion, kills"
    island_setup
    V=$OUT/v
    mkdir -p "$V/vessels"
    : >"$V/model-requests.jsonl"
    local port vessel pub out task seq before n

    # The Warden config first: island-socket reads its callers from it,
    # and the Vessel's writer key is named by its id.
    v_config "$WORK/v.json"
    vessel=$("$IBIN/island-socket" vessels --warden-config "$WORK/v.json" | awk '$2 == "v-prime" { print $1 }')
    "$IBIN/island-cli" keygen "$V/vessels/$vessel.key" >/dev/null
    pub=$("$IBIN/island-cli" public "$V/vessels/$vessel.key")
    "$IBIN/island-cli" grant --broker "http://127.0.0.1:$ISL_PORT" --root-key "$ISL/root.key" \
        --holder "$pub" --scope flare --rights verse >>"$OUT/island-init.out"

    port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
    python3 "$ISLAND/scripts/s4/scripted_model.py" --listen "127.0.0.1:$port" \
        --requests "$V/model-requests.jsonl" >"$V/model.out" 2>&1 &
    BG_PIDS+=("$!")
    V_SESSION=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
    "$IBIN/flare-server" --broker "http://127.0.0.1:$ISL_PORT" --root "$ISL_ROOT" --data "$V/flare" \
        --listen "127.0.0.1:$V_SESSION" --agent-socket "$V/agent.sock" --task-reader 0 --reply-sender 0 \
        >"$V/flare-server.out" 2>&1 &
    BG_PIDS+=("$!")
    for _ in $(seq 100); do [ -S "$V/agent.sock" ] && break; sleep 0.05; done
    [ -S "$V/agent.sock" ] || { echo "flare-server did not start; see $V/flare-server.out" >&2; return 1; }
    printf '%s\n' \
        "socket = \"$ROOTFS/run/island/island.sock\"" \
        "warden_config = \"$WORK/v.json\"" \
        "state_dir = \"$V/socket-state\"" \
        "broker = \"http://127.0.0.1:$ISL_PORT\"" \
        "root = \"$ISL_ROOT\"" \
        "key = \"$ISL/socket.key\"" \
        "access_every_secs = 2" \
        "tasks = \"$V/agent.sock\"" \
        "instructions = \"$V/no-instructions.md\"" \
        "default = \"scripted\"" \
        "[provider.scripted]" \
        "base_url = \"http://127.0.0.1:$port/v1\"" \
        "models = [\"scripted\"]" \
        "budget = { calls = 1000, bytes = 100000000 }" >"$V/socket.toml"
    mkdir -p "$ROOTFS/run/island"
    v_socket_up
    "$IBIN/island-sender" --senders "$WORK/run/senders" --broker "http://127.0.0.1:$ISL_PORT" \
        --root "$ISL_ROOT" --key "$ISL/sender.key" --session-socket "$V/agent.sock" \
        --vessel-keys "$V/vessels" --interval-ms 200 >"$V/sender.out" 2>"$V/sender.err" &
    BG_PIDS+=("$!")

    v_warden v-1 || { check v "flare-agent starts in its Vessel" no yes; return 0; }
    check v "whoami: the island socket knows the prime, with its six fork slots" \
        "$(v_whoami role) $(v_whoami forks_left)" "prime 6"

    # v1: a task answered through the session
    out=$(vsay gate-1 "hello from the gate")
    task=$(vtask gate-1)
    check v "v1: a message said in the session is answered there" \
        "$(grep -c "agent (to #$task): I wrote hello.txt for: hello from the gate" <<<"$out" || true)" 1
    check v "v1: the agent wrote the file in its Vessel" \
        "$(cat "$(vdir)/hello.txt" 2>/dev/null || echo missing)" "hello from the gate"
    check v "v1: the reply is in the session once" "$(sx "len([r for r in R if r['task'] == $task])")" 1

    # v2: the Warden killed while a task is in work
    before=$(wc -l <"$V/model-requests.jsonl")
    vsay gate-2 "write this slowly" 180 >"$V/say-2.out" &
    local saying=$!
    for _ in $(seq 300); do [ "$(wc -l <"$V/model-requests.jsonl")" -gt "$before" ] && break; sleep 0.1; done
    killw "$V_WARDEN"
    v_warden v-2 || { check v "v2: flare-agent starts again after the kill" no yes; return 0; }
    wait "$saying" || true
    task=$(vtask gate-2)
    check v "v2: killed in a task, the next start works it again and replies" \
        "$(grep -c "agent (to #$task): I wrote hello.txt for: write this slowly" "$V/say-2.out" || true)" 1
    check v "v2: the reply is in the session once" "$(sx "len([r for r in R if r['task'] == $task])")" 1
    note v "v2: the agent's starts and turns: $(vlog '[(e["event"], e.get("turn"), e.get("cursor")) for e in d if e["event"] in ("start", "turn")][-4:]')"

    # v3: a fork, the notes, a promotion; a kill while the adopted verse waits
    out=$(vsay gate-3 "fork this two ways" 240)
    task=$(vtask gate-3)
    check v "v3: the promoted child's held answer is the reply to the task" \
        "$(grep -c "agent (to #$task): Child 2 drafted draft-2.md." <<<"$out" || true)" 1
    check v "v3: each child said it started and what it made" \
        "$(sx "sorted((n['child'], n['done']) for n in N)")" \
        '[["v-prime-fork-41", false], ["v-prime-fork-41", true], ["v-prime-fork-42", false], ["v-prime-fork-42", true]]'
    vlog_wait 60 'd[-1]["event"] == "promoted"' || true
    check v "v3: the prime is started again on the promoted child's files" \
        "$(vlog '[e["child"] for e in d if e["event"] == "promoted"]')" '["v-prime-fork-42"]'
    check v "v3: its files are the promoted child's, not the other's" \
        "$(ls "$(vdir)" 2>/dev/null | grep -c '^draft-[12].md$' || true) $(head -c 20 "$(vdir)/draft-2.md" 2>/dev/null || true)" \
        "1 Draft it as a list, "
    check v "v3: the fork set's task file is gone from them" \
        "$([ -e "$(vdir)/.flare/fork-tasks.json" ] && echo there || echo gone)" gone
    check v "v3: two fork slots are used" "$(v_whoami forks_left)" 4
    swait 30 "len(W) == 1" || true
    check v "v3: the child's verse was adopted and waits for approval" \
        "$(sx "[e['payload']['shown'] for e in W]")" '["a verse on flare: [decision] \"Fork child 2 drafted it as a list\""]'
    check v "v3: nothing is written before the approval" "$(v_verses "$pub")" 0
    killw "$V_WARDEN"
    v_warden v-3 || { check v "v3: flare-agent starts again after the kill" no yes; return 0; }
    sleep 2
    check v "v3: after a Warden kill the verse still waits, shown once, and nothing was sent" \
        "$(sx "len(W)") $(sx "len(A)")" "1 0"
    seq=$(sx "(W or [{'seq': -1}])[0]['seq']")
    printf '/approve %s\n' "$seq" | timeout 60 "$IBIN/flare-tui" --server "ws://127.0.0.1:$V_SESSION/session" \
        --session main --client gate-approver --root "$ISL_ROOT" --broker "http://127.0.0.1:$ISL_PORT" \
        --replica "$V/replica-approver.log" --key "$ISL/approver.key" --reply-wait 1 >"$V/approve.out" 2>&1 || true
    swait 60 "len(A) == 1" || true
    check v "v3: approved, it is sent" "$(sx "[e['payload']['settled'] for e in A]")" '["sent"]'
    check v "v3: the verse is in the log once, signed with the Vessel's key" \
        "$(v_verses "$pub") $(v_verses "$pub" "Fork child 2 drafted it as a list")" "1 1"
    out=$(vsay gate-3b "and after the promotion")
    task=$(vtask gate-3b)
    check v "v3: with its effect authority back, the agent answers the next task" \
        "$(grep -c "agent (to #$task): I wrote hello.txt for: and after the promotion" <<<"$out" || true)" 1

    # v4: the Warden killed while the children of a fork work
    n=$(sx "len(N)")
    vsay gate-4 "fork this two ways, slowly" 300 >"$V/say-4.out" &
    saying=$!
    swait 120 "len([x for x in N[$n:] if not x['done']]) == 2" || true
    killw "$V_WARDEN"
    v_warden v-4 || { check v "v4: flare-agent starts again after the kill" no yes; return 0; }
    wait "$saying" || true
    task=$(vtask gate-4)
    check v "v4: killed in a fork, the prime waits on from the fork's Seal and answers without the lost children" \
        "$(grep -c "agent (to #$task): No child finished; nothing was promoted." "$V/say-4.out" || true)" 1
    check v "v4: the reply is in the session once" "$(sx "len([r for r in R if r['task'] == $task])")" 1
    check v "v4: no child was forked a second time: four slots used" "$(v_whoami forks_left)" 2
    check v "v4: nothing more was promoted" "$(vlog 'len([e for e in d if e["event"] == "promoted"])')" 1
    note v "v4: the children's notes: $(sx "[(x['child'], x['done']) for x in N[$n:]]")"

    # v5: island-socket killed while the children of a fork work
    n=$(sx "len(N)")
    vsay gate-5 "fork this two ways, slowly" 300 >"$V/say-5.out" &
    saying=$!
    swait 120 "len([x for x in N[$n:] if not x['done']]) >= 1" || true
    kill -KILL "$V_SOCKET" 2>/dev/null || true
    wait "$V_SOCKET" 2>/dev/null || true
    sleep 3
    v_socket_up
    wait "$saying" || true
    task=$(vtask gate-5)
    swait 200 "len({x['child'] for x in N[$n:] if x['done']}) == 2" || true
    check v "v5: island-socket killed in a fork: both children's last notes still arrive" \
        "$(sx "sorted({x['child'] for x in N[$n:] if x['done']})")" '["v-prime-fork-45", "v-prime-fork-46"]'
    check v "v5: the task is answered, once" "$(sx "len([r for r in R if r['task'] == $task])")" 1
    note v "v5: the reply: $(sx "[r['text'][:120] for r in R if r['task'] == $task]")"
    check v "every reply is in the session once, by its key" "$(sx "len(R) == len({r['key'] for r in R})")" true
    check v "every action waited and was settled at most once" \
        "$(sx "len(W) == len({e['payload']['request_hash'] for e in W}) and len(A) == len({e['payload']['request_hash'] for e in A})")" true
    check v "all six fork slots are used, each once" "$(v_whoami forks_left)" 0
    note v "island-sender's last words: $(tail -n 3 "$V/sender.err" 2>/dev/null | tr '\n' ' ')"
    killw "$V_WARDEN"
    kill "$V_SOCKET" 2>/dev/null || true
}

for r in $ONLY; do "run_$r"; done

echo
echo "== results ($OUT/results.tsv)"
column -t -s $'\t' "$OUT/results.tsv" || cat "$OUT/results.tsv"
if [ "$FAILS" -eq 0 ]; then
    echo "spike: PASS"
else
    echo "spike: FAIL ($FAILS check(s))"
    exit 1
fi
