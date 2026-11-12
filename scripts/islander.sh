#!/usr/bin/env bash
# islander.sh: start everything Island needs on this laptop, then open Flare.
#
#   ./islander.sh             start the services if needed, open a Flare terminal
#   ./islander.sh --after 0   the same, replaying the whole session first
#   ./islander.sh update      after merging a bundle: rebuild, reinstall, restart
#   ./islander.sh status      what is running
#   ./islander.sh stop        stop the services (the log and session stay on disk)
#   ./islander.sh logs        follow the services' output
#
# The helper for coding sessions and the replica run as user services, and
# so do the broker and the session server while they live on this laptop.
# Once they have moved to the Island server (docs/server.md), the settings
# name them in ISLAND_BROKER and ISLAND_SESSION, and only the helper and the
# replica run here. The first run installs the services.
set -euo pipefail

REPO=${ISLAND_REPO:-/srv/oc-projects/island}
KEY=${ISLAND_KEY:-$REPO/.island/daily.key}
CLIENT=${ISLAND_CLIENT:-$(hostname -s | cut -c1-32)}
BIN=$HOME/.local/bin
ENV_FILE=$HOME/.config/island/island.env
LOCAL_UNITS=(island-broker island-helper flare-link flare-server)
REMOTE_UNITS=(island-helper flare-link)

# The settings, if written: where the broker and the session server are.
BROKER=http://127.0.0.1:4747
SESSION=ws://127.0.0.1:4749/session
if [[ -f $ENV_FILE ]]; then
    BROKER=$(sed -n 's/^ISLAND_BROKER=//p' "$ENV_FILE" | tail -1)
    SESSION=$(sed -n 's/^ISLAND_SESSION=//p' "$ENV_FILE" | tail -1)
    BROKER=${BROKER:-http://127.0.0.1:4747}
    SESSION=${SESSION:-ws://127.0.0.1:4749/session}
fi
if [[ $BROKER == http://127.0.0.1:* ]]; then
    UNITS=("${LOCAL_UNITS[@]}")
else
    UNITS=("${REMOTE_UNITS[@]}")
fi

say() { printf 'islander: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }

# answers PORT [HOST]: something accepts connections there.
answers() { (exec 3<>"/dev/tcp/${2:-127.0.0.1}/$1") 2>/dev/null; }

# host:port of a URL such as http://10.77.0.1:4747 or ws://10.77.0.1:4749/session
hostport() { local rest=${1#*://}; printf '%s\n' "${rest%%/*}"; }

installed() {
    [[ -x $BIN/flare-tui && -f $ENV_FILE ]] || return 1
    local unit
    for unit in "${REMOTE_UNITS[@]}"; do
        [[ -f $HOME/.config/systemd/user/$unit.service ]] || return 1
    done
}

install_everything() {
    [[ -f $REPO/.island/root.key ]] || die "no $REPO/.island/root.key: run \`just init\` in $REPO first"
    say "building and installing the programs (this takes a while the first time)"
    (cd "$REPO" && just install)
    install -d "$HOME/.config/island" "$HOME/.config/systemd/user"
    if [[ ! -f $ENV_FILE ]]; then
        local root
        root=$("$BIN/island-cli" public "$REPO/.island/root.key")
        umask 077
        cat > "$ENV_FILE" <<END
ISLAND_LOG=$REPO/.island/island.log
ISLAND_APPENDER_KEY=$REPO/.island/appender.key
ISLAND_SESSION_KEY=$REPO/.island/opencode.key
ISLAND_ROOT=$root
ISLAND_BROKER=http://127.0.0.1:4747
END
        say "settings written to $ENV_FILE"
    fi
    install -m 644 "$REPO"/deploy/user/*.service "$HOME/.config/systemd/user/"
    systemctl --user daemon-reload
    systemctl --user enable "${UNITS[@]}" >/dev/null 2>&1
}

start_services() {
    if [[ ${UNITS[*]} == "${REMOTE_UNITS[*]}" ]]; then
        start_remote
        return
    fi
    # A broker started by hand holds the log; the service could not open it.
    if answers 4747 && ! systemctl --user is-active --quiet island-broker; then
        die "something else is serving port 4747, probably a \`just broker\` started by hand: stop it (Ctrl-C) and run this again"
    fi
    if answers 4749 && ! systemctl --user is-active --quiet flare-server; then
        die "something else is serving port 4749, probably a \`just session\` started by hand: stop it (Ctrl-C) and run this again"
    fi
    systemctl --user start "${UNITS[@]}"
    local port name
    for port in 4747:broker 4748:helper 4749:session-server; do
        name=${port#*:}; port=${port%%:*}
        for _ in $(seq 50); do
            answers "$port" && break
            sleep 0.1
        done
        answers "$port" || die "the $name did not come up on port $port: see \`./islander.sh logs\`"
    done
}

# The broker and the session server are on the Island server: only the
# helper and the replica run here, and the server must answer.
start_remote() {
    if systemctl --user is-active --quiet island-broker; then
        die "a broker is running on this laptop, but the settings name $BROKER: the log has one broker; stop this one (systemctl --user disable --now island-broker flare-server)"
    fi
    systemctl --user start "${UNITS[@]}"
    local target host port
    for target in "$BROKER" "$SESSION"; do
        target=$(hostport "$target"); host=${target%:*}; port=${target##*:}
        answers "$port" "$host" || die "the Island server does not answer at $target: is WireGuard up? (sudo systemctl start wg-quick@wg-island; ping $host)"
    done
}

open_flare() {
    [[ -f $KEY ]] || die "no key at $KEY (set ISLAND_KEY to another one)"
    local root
    root=$("$BIN/island-cli" public "$REPO/.island/root.key")
    exec "$BIN/flare-tui" --client "$CLIENT" --key "$KEY" --root "$root" \
        --broker "$BROKER" --server "$SESSION" "$@"
}

case ${1:-} in
    update)
        install_everything
        systemctl --user restart "${UNITS[@]}"
        say "updated and restarted"
        ;;
    status)
        systemctl --user status "${UNITS[@]}" --no-pager || true
        ;;
    stop)
        systemctl --user stop "${UNITS[@]}"
        say "stopped; the log, the replica and the session are kept"
        ;;
    logs)
        args=()
        for unit in "${UNITS[@]}"; do args+=(-u "$unit"); done
        exec journalctl --user -f "${args[@]}"
        ;;
    help|-h|--help)
        sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
        ;;
    *)
        installed || install_everything
        start_services
        open_flare "$@"
        ;;
esac
