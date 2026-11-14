#!/usr/bin/env bash
# s2-spike.sh: spikes S2 and S4. An Island agent in Taso Vessels on
# this host, checked against what the S2 plan read in Taso's code.
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
#      kill and a broker restart: the next pass reports the same mark
#      (no second send), lapses the unapproved effect, and the prime
#      gets its effect authority back
#   f  the prime has a released Irreversible effect and its Warden is
#      killed before a sender reports: after the restart the effect is
#      listed unknown, a new effect is refused (no effect authority), and
#      the lapse rule of decision D1 (the sender reports NotApplied)
#      restores authority
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
    case $r in a|b|c|d|e|f|s|p) ;; *) echo "ONLY: unknown run $r (a b c d e f s p; b also does g and h)" >&2; exit 2 ;; esac
done

echo "== building Taso (release) in $TASO at $(git -C "$TASO" log -1 --format='%h %s' 2>/dev/null || echo '?')"
(cd "$TASO" && cargo build --release -q -p taso-warden -p taso-landlock \
    && cargo build --release -q -p taso-blk --bin taso-blkd)
BIN=$TASO/target/release
case " $ONLY " in *" s "*)
    echo "== building island-cli, island-broker, island-sender (release) in $ISLAND"
    (cd "$ISLAND" && cargo build --release -q --locked -p island-cli -p island-broker -p island-sender)
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
    for p in "${BG_PIDS[@]}"; do kill -KILL "$p" 2>/dev/null || true; done
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
    ISL=$WORK/island
    mkdir -p "$ISL"
    local who
    for who in root appender approver sender; do "$IBIN/island-cli" keygen "$ISL/$who.key" >/dev/null; done
    ISL_ROOT=$("$IBIN/island-cli" public "$ISL/root.key")
    "$IBIN/island-cli" init --log "$ISL/island.log" --appender-key "$ISL/appender.key" \
        --root-key "$ISL/root.key" >"$OUT/island-init.out"
    local right
    for who in approver:approval sender:dispatch; do
        right=${who#*:} who=${who%%:*}
        "$IBIN/island-cli" grant --log "$ISL/island.log" --appender-key "$ISL/appender.key" \
            --root-key "$ISL/root.key" --holder "$("$IBIN/island-cli" public "$ISL/$who.key")" \
            --scope approvals --rights "$right" >>"$OUT/island-init.out"
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
    echo "-- s: island-sender: approvals, a crash after a send, the lapse rule"
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
    check s "the sent one is reported from its dispatch, the unapproved one lapses" \
        "$(jq_ "$p2" "[(x['action'], x['unknown'], x.get('reported')) for x in d]")" \
        '[["already-sent", true, true], ["lapse", true, true]]'
    check s "the same dispatch as before the crash: no second send" \
        "$(jq_ "$p2" "d[0].get('sent_at')")" "$(jq_ "$crash" "d[0]['sent_at']")"
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
