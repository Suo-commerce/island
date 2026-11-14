#!/usr/bin/env bash
# check.sh: is the Taso node up, and does it come back from kills?  Root,
# on the server, from the Island checkout (docs/server.md, "6. The Taso
# node"):
#
#   scripts/node/check.sh          what runs; flare-home at the end of its
#                                  plan; its one action proposed once and
#                                  dispatched at most once
#   scripts/node/check.sh kills    the same, then each kill in turn: the
#                                  Warden, island-sender, the block log.
#                                  After each, everything is running again,
#                                  flare-home started again at its last Seal,
#                                  and nothing was proposed or sent twice
#
# After a reboot, the plain check is the reboot test: a reboot is a crash.
#
# The stand-in agent journals to /work/.flare-journal.jsonl in its dataset,
# under /var/lib/taso/mnt.  What it wrote after its last Seal goes at its
# next start, so the file always shows the Seal's history plus this start.

set -euo pipefail

ISLAND=$(cd "$(dirname "$0")/../.." && pwd)
CLI=$ISLAND/target/release/island-cli
ROOT=$(sed -n 's/^ISLAND_ROOT=//p' /etc/island/island.env)
BROKER=http://10.77.0.1:4747
MNT=/var/lib/taso/mnt
UNITS=(taso-blkd taso-wardend island-sender)
IDLE_AT=4   # the plan's last step: mark, seal, effect, seal, idle

[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 2; }
[ -x "$CLI" ] || { echo "build first: scripts/node/install.sh" >&2; exit 2; }

FAILS=0
check() {   # check <what> <got> <want>
    local verdict=PASS
    [ "$2" = "$3" ] || { verdict=FAIL; FAILS=$((FAILS + 1)); }
    printf '%s\t%s\tgot=%s\twant=%s\n' "$verdict" "$1" "$2" "$3"
}
note() { printf 'NOTE\t%s\n' "$1"; }

journal() { find "$MNT" -maxdepth 12 -name .flare-journal.jsonl 2>/dev/null | head -1; }

# agent <expression over d (events) and S (the last start's events)>
agent() {
    local j; j=$(journal)
    [ -n "$j" ] || { echo "no-journal"; return 0; }
    python3 - "$j" "$1" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    d = [json.loads(line) for line in fh if line.strip()]
starts = [e for e in d if e["step"] == "start"]
S = [e for e in d if starts and e["start"] == starts[-1]["start"]]
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
at_idle() { [ "$(agent '[e["step"] for e in S][-1:] == ["idle"]')" = true ]; }
new_start() { [ "$(agent 'S[0]["start"] if S else ""')" != "$1" ] && at_idle; }

dispatches() {   # dispatch entries for flare-home in the log
    local v; v=$(basename "$(ls /run/taso/run/senders/*.sock 2>/dev/null | head -1)" .sock)
    [ -n "$v" ] || { echo "no-socket"; return 0; }
    "$CLI" tail --broker "$BROKER" --root "$ROOT" -n 1000 | grep -c " dispatch .* vessel $v op " || true
}

state() {   # the plain checks
    local when=$1
    wait_until 60 all_active || true
    for u in "${UNITS[@]}"; do check "$when: $u is running" "$(systemctl is-active "$u" || true)" active; done
    check "$when: the Warden's sender socket for flare-home is there" \
        "$(ls /run/taso/run/senders/*.sock 2>/dev/null | wc -l)" 1
    wait_until 180 at_idle || true
    check "$when: flare-home is at the end of its plan" "$(agent '[e["step"] for e in S][-1:]')" '["idle"]'
    check "$when: it proposed its action once, in all its history" \
        "$(agent 'len([e for e in d if e["step"] == "effect"])')" 1
    check "$when: the dispatch entries for it in the log: at most one" \
        "$(dispatches | awk '{ print ($1 <= 1) ? "ok" : $1 }')" ok
    note "$when: flare-home's starts in its Seal's history and now: $(agent '[(e["start"], e["resume_at"], e["last_sealed"]) for e in d if e["step"] == "start"]')"
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
                check "after killing $unit: flare-home started again at its last Seal" \
                    "$(agent 'S[0]["resume_at"]')" "$IDLE_AT"
            else
                check "after killing $unit: flare-home started again" "no new start" "a new start"
            fi
            ;;
        island-sender)
            wait_until 60 systemctl is-active -q island-sender || true
            check "after killing island-sender: flare-home's run went on" \
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
        for unit in taso-wardend island-sender taso-blkd; do kill_one "$unit"; done
        ;;
    *)
        sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
        exit 2
        ;;
esac

echo
if [ "$FAILS" -eq 0 ]; then echo "node: PASS"; else echo "node: FAIL ($FAILS check(s))"; exit 1; fi
