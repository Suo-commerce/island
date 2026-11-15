#!/usr/bin/env bash
# check.sh: is the Taso node up, and does it come back from kills?  Root,
# on the server, from the Island checkout (docs/server.md, "6. The Taso
# node"):
#
#   scripts/node/check.sh          what runs; flare-agent started and
#                                  carrying on; the old stand-in's action
#                                  dispatched at most once; every reply
#                                  in the session once
#   scripts/node/check.sh kills    the same, then each kill in turn: the
#                                  Warden, island-sender, island-socket,
#                                  flare-server, the block log.
#                                  After each, everything is running again,
#                                  flare-agent started again on its last
#                                  Seal (or ran on), and nothing was sent
#                                  twice
#
#   scripts/node/check.sh think [MODEL]
#                                  the same, then one model call through the
#                                  island socket as flare-home (MODEL, or the
#                                  config's default), and its `access` entry
#                                  in the log (within 45 s)
#
#   scripts/node/check.sh converse [TEXT]
#                                  the same, then a conversation: TEXT said
#                                  to the agent in the session main (with
#                                  flare-tui), and its reply back as an
#                                  agent_text, once
#
# After a reboot, the plain check is the reboot test: a reboot is a crash.
#
# The island socket is asked as flare-home's own uid (setpriv), the way its
# agent reaches it from inside the Vessel; root is no Vessel and is refused.
#
# flare-agent notes its starts, tasks and turns in /work/.flare/log.jsonl
# in its dataset, under /var/lib/taso/mnt.  What it wrote after its last
# Seal goes at its next start, so the file always shows the Seal's history
# plus this start.

set -euo pipefail

ISLAND=$(cd "$(dirname "$0")/../.." && pwd)
CLI=$ISLAND/target/release/island-cli
ROOT=$(sed -n 's/^ISLAND_ROOT=//p' /etc/island/island.env)
BROKER=http://10.77.0.1:4747
MNT=/var/lib/taso/mnt
UNITS=(taso-blkd taso-wardend island-sender island-stub-model island-socket flare-server)
ISOCK=/var/lib/taso/agent-island/run/island/island.sock
SESSION=/var/lib/island/flare/sessions/main.jsonl
TUI=$ISLAND/target/release/flare-tui
# flare-home's first host uid: what the island socket sees when it asks.
FLARE_UID=$(python3 -c '
import json
c = json.load(open("/etc/taso/warden.json"))
v = [v for v in c["vessels"] if v["subject"] == "flare-home"][0]
print(c["runtime"]["host_uid_base"] + v["uid_slot"] * 65536)')

# island <path> [curl options]: ask the island socket as flare-home
island() {
    local path=$1; shift
    setpriv --reuid="$FLARE_UID" --regid="$FLARE_UID" --clear-groups \
        curl -sS --max-time 30 --unix-socket "$ISOCK" "$@" "http://island$path"
}

[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
[ -x "$CLI" ] || { echo "build first: scripts/node/install.sh" >&2; exit 2; }

FAILS=0
check() {   # check <what> <got> <want>
    local verdict=PASS
    [ "$2" = "$3" ] || { verdict=FAIL; FAILS=$((FAILS + 1)); }
    printf '%s\t%s\tgot=%s\twant=%s\n' "$verdict" "$1" "$2" "$3"
}
note() { printf 'NOTE\t%s\n' "$1"; }

agent_log() { find "$MNT" -maxdepth 12 -path '*/.flare/log.jsonl' 2>/dev/null | head -1; }

# agent <expression over d (events) and S (the last start and after)>
agent() {
    local j; j=$(agent_log)
    [ -n "$j" ] || { echo "no-log"; return 0; }
    python3 - "$j" "$1" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    d = [json.loads(line) for line in fh if line.strip()]
starts = [i for i, e in enumerate(d) if e["event"] == "start"]
S = d[starts[-1]:] if starts else []
r = eval(sys.argv[2])
print(json.dumps(r) if isinstance(r, (list, dict, bool)) or r is None else r)
PY
}

# replies <expression over R (the agent_text payloads in the session)>
replies() {
    [ -f "$SESSION" ] || { echo "no-session"; return 0; }
    python3 - "$SESSION" "$1" <<'PY'
import json, sys
R = [e["payload"] for e in map(json.loads, open(sys.argv[1])) if e.get("kind") == "agent_text"]
r = eval(sys.argv[2])
print(json.dumps(r) if isinstance(r, (list, dict, bool)) or r is None else r)
PY
}

wait_until() {   # wait_until <seconds> <command...>: until the command succeeds
    local secs=$1; shift
    for _ in $(seq $((secs * 5))); do "$@" >/dev/null 2>&1 && return 0; sleep 0.2; done
    return 1
}
all_active() { for u in "${UNITS[@]}"; do systemctl is-active -q "$u" || return 1; done; }
started() { [ "$(agent 'bool(S)')" = true ]; }
# A start other than $1, read from the log: while the block log comes back
# the dataset is not mounted, and "no log" is not a new start.
new_start() {
    local now; now=$(agent 'S[0]["start"] if S else ""')
    [ -n "$now" ] && [ "$now" != no-log ] && [ "$now" != "$1" ]
}

dispatches() {   # dispatch entries for flare-home in the log
    local v; v=$(basename "$(ls /run/taso/run/senders/*.sock 2>/dev/null | head -1)" .sock)
    [ -n "$v" ] || { echo "no-socket"; return 0; }
    "$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep -c " dispatch .* vessel $v op " || true
}

state() {   # the plain checks
    local when=$1
    wait_until 60 all_active || true
    for u in "${UNITS[@]}"; do check "$when: $u is running" "$(systemctl is-active "$u" || true)" active; done
    # The socket comes with the Vessel's start, after the units: wait for
    # the agent first.
    wait_until 180 started || true
    check "$when: the Warden's sender socket for flare-home is there" \
        "$(ls /run/taso/run/senders/*.sock 2>/dev/null | wc -l)" 1
    check "$when: flare-agent started in flare-home" "$(agent 'bool(S)')" true
    note "$when: its starts in its Seal's history and now: $(agent '[(e["start"], e["turn"], e["cursor"]) for e in d if e["event"] == "start"]')"
    note "$when: its last turns: $(agent '[(e["turn"], e["cursor"]) for e in d if e["event"] == "turn"][-3:]')"
    check "$when: the dispatch entries for it in the log: at most one" \
        "$(dispatches | awk '{ print ($1 <= 1) ? "ok" : $1 }')" ok
    check "$when: every reply in the session once, by its key" \
        "$(replies 'len(R) == len({r["key"] for r in R})')" true
    wait_until 30 test -S "$ISOCK" || true
    check "$when: the island socket says flare-home is flare-home" \
        "$(island /whoami | python3 -c 'import json, sys; d = json.load(sys.stdin); print(d["subject"], d["role"], d["vessel"])' 2>/dev/null || echo no-answer)" \
        "flare-home prime $(basename "$(ls /run/taso/run/senders/*.sock 2>/dev/null | head -1)" .sock)"
    check "$when: the island socket refuses root, which is no Vessel" \
        "$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 --unix-socket "$ISOCK" http://island/whoami || true)" 403
    note "$when: island-sender's last word on it: $(journalctl -u island-sender -o cat -n 200 --no-pager | grep '^{' | tail -1)"
}

kill_one() {   # kill_one <unit>: SIGKILL it, then the plain checks
    local unit=$1 before
    before=$(agent 'S[0]["start"] if S else ""')
    echo "-- SIGKILL $unit"
    systemctl kill -s SIGKILL "$unit"
    sleep 1
    case $unit in
        taso-wardend|taso-blkd)
            if wait_until 240 new_start "$before"; then
                check "after killing $unit: flare-agent started again on its last Seal" \
                    "$(agent 'S[0]["cursor"] == ([e["cursor"] for e in d[:-len(S)] if e["event"] == "turn"] or [None])[-1]')" true
            else
                check "after killing $unit: flare-agent started again" "no new start" "a new start"
            fi
            ;;
        island-sender|island-socket|flare-server)
            wait_until 60 systemctl is-active -q "$unit" || true
            check "after killing $unit: flare-home's run went on" \
                "$(agent 'S[0]["start"] if S else ""')" "$before"
            ;;
    esac
    state "after killing $unit"
}

case ${1:-} in
    "")
        state now
        ;;
    kills)
        state before
        for unit in taso-wardend island-sender island-socket flare-server taso-blkd; do kill_one "$unit"; done
        ;;
    think)
        state now
        vessel=$(basename "$(ls /run/taso/run/senders/*.sock 2>/dev/null | head -1)" .sock)
        before=$("$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep -c " access .*vessel $vessel " || true)
        body=$(python3 -c 'import json, sys
d = {"messages": [{"role": "user", "content": "hello from check.sh: answer in one short sentence"}]}
if sys.argv[1]: d["model"] = sys.argv[1]
print(json.dumps(d))' "${2:-}")
        answer=$(island /v1/chat/completions --max-time 120 -H 'content-type: application/json' -d "$body" || true)
        check "think: a model answers flare-home through the island socket" \
            "$(python3 -c 'import json, sys; d = json.loads(sys.argv[1]); print("choices" in d)' "$answer" 2>/dev/null || echo "$answer" | head -c 300)" True
        note "think: $(python3 -c 'import json, sys
d = json.loads(sys.argv[1])
print(d.get("model"), "said:", repr(d["choices"][0]["message"]["content"][:160]))' "$answer" 2>/dev/null || true)"
        accessed() { [ "$("$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep -c " access .*vessel $vessel " || true)" -gt "$before" ]; }
        wait_until 45 accessed || true
        check "think: the call is in the log as an access entry for flare-home" \
            "$("$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep -c " access .*vessel $vessel " || true)" "$((before + 1))"
        note "think: $("$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep " access .*vessel $vessel " | tail -1)"
        ;;
    converse)
        state now
        text=${2:-"check.sh asks: answer in one short sentence, what can you do?"}
        before=$(replies 'len(R)')
        inference=$("$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep -c ' access .*Inference' || true)
        out=$(printf 'say %s\n' "$text" | timeout 300 "$TUI" --server ws://10.77.0.1:4749/session \
            --session main --client check --after 999999999 --root "$ROOT" --broker "$BROKER" \
            --replica /tmp/check-flare-replica.log --reply-wait 240 2>&1 || true)
        task=$(printf '%s\n' "$out" | sed -n 's/^#\([0-9]*\) .* check → agent: .*/\1/p' | tail -1)
        check "converse: said in the session, as a task" "$([ -n "$task" ] && echo yes || echo no)" yes
        reply=$(printf '%s\n' "$out" | grep -A20 " agent (to #${task:-x}): " || true)
        check "converse: the agent replied to it" "$([ -n "$reply" ] && echo yes || echo no)" yes
        note "converse: $(printf '%s' "$reply" | head -c 600)"
        check "converse: the reply is in the session once" \
            "$(replies "len([r for r in R if r['task'] == ${task:-0}])")" 1
        check "converse: every reply in the session once, by its key" \
            "$(replies 'len(R) == len({r["key"] for r in R})')" true
        moved() { [ "$("$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep -c ' access .*Inference' || true)" -gt "$inference" ]; }
        wait_until 45 moved || true
        check "converse: its model calls are in the log as access entries" "$(moved && echo yes || echo no)" yes
        note "converse: $("$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep ' access ' | tail -2 | tr '\n' '|')"
        note "converse: the agent's turns: $(agent '[(e["turn"], e["cursor"]) for e in d if e["event"] == "turn"][-3:]')"
        ;;
    *)
        sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
        exit 2
        ;;
esac

echo
if [ "$FAILS" -eq 0 ]; then echo "node: PASS"; else echo "node: FAIL ($FAILS check(s))"; exit 1; fi
