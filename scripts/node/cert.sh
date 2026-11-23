#!/usr/bin/env bash
# cert.sh: the face's certificate (docs/server.md, "The face over HTTPS").
# Run as root, on the server, from the Island checkout, in this order,
# once:
#
#   scripts/node/cert.sh setup NAME   the ACME client (lego, a pinned
#                                     release, checked by its hash), the
#                                     user island-cert, the directories,
#                                     the units
#   scripts/node/cert.sh token        the narrow deSEC token (desec.py);
#                                     prints the two NS records to add
#                                     where the domain's DNS is
#   scripts/node/cert.sh dns          is the name delegated and answering?
#   scripts/node/cert.sh try          a certificate from Let's Encrypt's
#                                     staging side: proves the whole path
#                                     and is thrown away
#   scripts/node/cert.sh run          the real one, into /etc/island/tls
#   scripts/node/cert.sh on           tells flare-server, opens 443 inside
#                                     the tunnel, turns the daily renewal on
#
# and whenever: `cert.sh status`, and `cert.sh verify` (asks deSEC to
# refuse the kept token everything but the proof record). Each step is
# safe to run again.
#
# The certificate is proved by a DNS record (the server has no public web
# port). The face's name is a zone of its own at deSEC; the token kept
# here, /etc/island/cert.env, can write that zone's one proof record and
# nothing else. Renewal is the unit island-cert@live.service, daily by
# island-cert.timer, as the user island-cert; lego renews when the
# certificate is due and otherwise does nothing. flare-server reads the
# files again when they change: no restart.
#
# `cert.sh issue live|staging` is what the unit runs.
#
# For the tests only: CERT_ENV CERT_STATE CERT_TLS CERT_LEGO CERT_SERVER
# CERT_LEGO_EXTRA CERT_GROUP name other places and another CA, and
# DESEC_API a stand-in for deSEC.

set -euo pipefail

LEGO_VERSION=5.6.0
LEGO_SHA256_amd64=f02fc11b6f56b97e387fa7caa0268f86f5a336953b22458e6430e4651fea439a
LEGO_SHA256_arm64=34d87a6552d69390c4a4560237045c47713a8e148d899e16db0e97040894ed1f

HERE=$(cd "$(dirname "$0")" && pwd)
LIB=/usr/local/lib/island
ENV=${CERT_ENV:-/etc/island/cert.env}
STATE=${CERT_STATE:-/var/lib/island-cert}
TLS=${CERT_TLS:-/etc/island/tls}
LEGO=${CERT_LEGO:-$LIB/lego}
GROUP=${CERT_GROUP:-island}
ADDRESS=10.77.0.1
FLARE_ENV=/etc/island/flare-tls.env
NFT=/etc/nftables.d/island.nft

say() { echo "== $*"; }
die() { echo "cert.sh: $*" >&2; exit 2; }
root() { [ "$(id -u)" = 0 ] || die "run as root"; }

name() {   # the face's name, from the env file (or the unit's environment)
    local n=${CERT_NAME:-}
    [ -n "$n" ] || n=$(sed -n 's/^CERT_NAME=//p' "$ENV" 2>/dev/null | tail -1)
    [ -n "$n" ] || die "no name yet: cert.sh setup NAME"
    echo "$n"
}

# What a certificate file says of itself.
ends() { openssl x509 -in "$1" -noout -enddate | cut -d= -f2; }
issuer() { openssl x509 -in "$1" -noout -issuer | sed 's/^issuer=//'; }
covers() { openssl x509 -in "$1" -noout -checkhost "$2" | grep -q "does match"; }
pair() {   # pair <cert> <key>: the key is the certificate's
    [ "$(openssl x509 -in "$1" -noout -pubkey | openssl sha256)" = "$(openssl pkey -in "$2" -pubout | openssl sha256)" ]
}

setup() {
    root
    local n=${1:-} arch sum tmp
    [[ $n =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$ ]] || die "setup NAME, the face's name, like island.example.net"
    for t in curl tar sha256sum openssl python3 systemctl useradd runuser dig; do
        command -v "$t" >/dev/null || die "$t is missing$([ "$t" = dig ] && echo ' (apt install bind9-dnsutils)')"
    done
    getent group "$GROUP" >/dev/null || die "there is no group $GROUP yet: scripts/node/install.sh first"

    case $(uname -m) in
        x86_64) arch=amd64 sum=$LEGO_SHA256_amd64 ;;
        aarch64) arch=arm64 sum=$LEGO_SHA256_arm64 ;;
        *) die "no lego release is pinned for $(uname -m)" ;;
    esac
    if [ "$("$LEGO" --version 2>/dev/null || true)" = "lego version $LEGO_VERSION linux/$arch" ]; then
        say "lego $LEGO_VERSION is installed"
    else
        tmp=$(mktemp -d)
        curl -fsSL --max-time 300 -o "$tmp/lego.tar.gz" \
            "https://github.com/go-acme/lego/releases/download/v$LEGO_VERSION/lego_v${LEGO_VERSION}_linux_$arch.tar.gz" \
            || { rm -rf "$tmp"; die "could not download lego $LEGO_VERSION"; }
        echo "$sum  $tmp/lego.tar.gz" | sha256sum -c --quiet \
            || { rm -rf "$tmp"; die "the downloaded lego is not the pinned release (hash differs); nothing installed"; }
        tar -xzf "$tmp/lego.tar.gz" -C "$tmp" lego
        install -D -m 755 "$tmp/lego" "$LEGO"
        rm -rf "$tmp"
        say "installed $("$LEGO" --version) (hash checked)"
    fi

    if ! getent passwd island-cert >/dev/null; then
        useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin island-cert
        say "made the user island-cert"
    fi
    # The certificate and its key: written by island-cert, read by flare-server's group.
    install -d -m 750 -o island-cert -g "$GROUP" "$TLS"
    install -d -m 700 -o island-cert -g island-cert "$STATE"
    runuser -u island-cert -g island-cert -G "$GROUP" -- test -w "$TLS" \
        || die "island-cert cannot write $TLS (a directory above it is closed to it)"

    if [ -f "$ENV" ]; then
        if grep -q '^CERT_NAME=' "$ENV"; then
            sed -i "s/^CERT_NAME=.*/CERT_NAME=$n/" "$ENV"
        else
            echo "CERT_NAME=$n" >> "$ENV"
        fi
    else
        (umask 077; printf '%s\n' "# The face's certificate (scripts/node/cert.sh). Root only." "CERT_NAME=$n" > "$ENV")
    fi
    chmod 600 "$ENV"
    say "the name is $n ($ENV)"

    units
    say "next: scripts/node/cert.sh token"
}

units() {   # the programs the unit runs and the units; install.sh does the same
    install -D -m 755 "$HERE/cert.sh" "$LIB/cert.sh"
    install -D -m 755 "$HERE/desec.py" "$LIB/desec.py"
    install -m 644 "$HERE/../../deploy/server/island-cert@.service" "$HERE/../../deploy/server/island-cert.timer" /etc/systemd/system/
    systemctl daemon-reload
    say "units installed: island-cert@.service, island-cert.timer (the timer is turned on by cert.sh on)"
}

token() {
    root
    local n
    n=$(name)
    python3 "$HERE/desec.py" mint --name "$n" --address "$ADDRESS" --env "$ENV"
}

dns() {
    local n parent at delegated address caa ok=0
    n=$(name); parent=${n#*.}
    at=$(dig +short NS "$parent" | head -1 || true)
    [ -n "$at" ] || die "found no nameserver for $parent"
    # Asked of the parent's own server: no cache in between.
    delegated=$(dig +norecurse +noall +authority +answer NS "$n" "@$at" | awk '$4 == "NS" { print tolower($5) }' | sort | tr '\n' ' ' || true)
    case $delegated in
        *desec*) echo "PASS	$parent delegates $n to: $delegated" ;;
        *) echo "FAIL	$parent ($at) does not delegate $n to deSEC yet (it answers: ${delegated:-nothing}). Add the two NS records; they show within minutes."; ok=1 ;;
    esac
    address=$(dig +short A "$n" @ns1.desec.io | tr '\n' ' ' || true)
    if [ "$address" = "$ADDRESS " ]; then
        echo "PASS	deSEC answers $n A $ADDRESS"
    else
        echo "FAIL	deSEC answers $n A: ${address:-nothing} (want $ADDRESS): cert.sh token writes it"; ok=1
    fi
    address=$(dig +short A "$n" | tr '\n' ' ' || true)
    if [ "$address" = "$ADDRESS " ]; then
        echo "PASS	this server's resolver finds $n A $ADDRESS"
    else
        echo "NOTE	this server's resolver finds $n A: ${address:-nothing yet} (an earlier 'no such name' can stay cached for up to an hour; the certificate does not wait for it)"
    fi
    caa=$(dig +short CAA "$n" || true; dig +short CAA "$parent" || true)
    if [ -n "$caa" ] && ! grep -q letsencrypt.org <<<"$caa"; then
        echo "FAIL	a CAA record keeps Let's Encrypt out: $(tr '\n' ' ' <<<"$caa")"; ok=1
    fi
    return $ok
}

issue() {   # what the unit runs, as island-cert: issue live|staging
    local mode=${1:-} server n
    case $mode in
        live) server=letsencrypt ;;
        staging) server=letsencrypt-staging ;;
        *) die "issue live|staging" ;;
    esac
    n=$(name)
    # lego's part is ACME; the one DNS write is desec.py's (present,
    # cleanup), so what the kept token is used for is all in this checkout.
    # It waits until deSEC's own servers answer with the record.
    export CERT_NAME=$n EXEC_PATH=$HERE/desec.py EXEC_PROPAGATION_TIMEOUT=300 EXEC_POLLING_INTERVAL=5
    # shellcheck disable=SC2086
    "$LEGO" --log.format text run --accept-tos --server "${CERT_SERVER:-$server}" --path "$STATE/$mode" \
        --account-id island --domains "$n" --dns exec \
        --dns.propagation.disable-rns --no-random-sleep ${CERT_LEGO_EXTRA:-}
    if [ "$mode" = live ]; then deploy "$n"; else say "staging: a certificate came; it is not put anywhere"; fi
}

deploy() {   # the live certificate into $TLS, whole files, the key first
    local n=$1 cert key
    cert=$STATE/live/certificates/$n.crt key=$STATE/live/certificates/$n.key
    [ -s "$cert" ] && [ -s "$key" ] || die "lego left no certificate at $cert"
    covers "$cert" "$n" || die "the certificate is not for $n"
    pair "$cert" "$key" || die "the certificate and the key do not belong together"
    if cmp -s "$cert" "$TLS/fullchain.pem" && cmp -s "$key" "$TLS/privkey.pem"; then
        say "the certificate in $TLS is the current one (until $(ends "$cert"))"
        return
    fi
    install -m 640 -g "$GROUP" "$key" "$TLS/.privkey.pem.new"
    install -m 640 -g "$GROUP" "$cert" "$TLS/.fullchain.pem.new"
    mv -f "$TLS/.privkey.pem.new" "$TLS/privkey.pem"
    mv -f "$TLS/.fullchain.pem.new" "$TLS/fullchain.pem"
    say "put the certificate in $TLS: for $n, from $(issuer "$cert"), until $(ends "$cert")"
}

started() {   # started <mode>: run the unit, show what it said, say how it ended
    root
    local mode=$1 unit since
    unit=island-cert@$mode.service
    grep -q '^DESEC_TOKEN=' "$ENV" 2>/dev/null || die "no token yet: cert.sh token"
    since=$(date '+%Y-%m-%d %H:%M:%S')
    say "running $unit (a minute or two)"
    if systemctl start "$unit"; then
        journalctl -u "$unit" --since "$since" --no-pager -o cat | tail -25
        return 0
    fi
    journalctl -u "$unit" --since "$since" --no-pager -o cat | tail -40
    die "$unit failed; its words are above (all of them: journalctl -u $unit)"
}

try() {
    started staging
    say "the staging run passed: the whole path works. Next: scripts/node/cert.sh run"
}

run() {
    local n
    started live
    n=$(name)
    runuser -u island -- test -r "$TLS/fullchain.pem" && runuser -u island -- test -r "$TLS/privkey.pem" \
        || die "the user island (flare-server) cannot read the files in $TLS"
    say "flare-server can read it. Next, the first time: scripts/node/cert.sh on"
}

serves() {   # serves <name>: the page over https, with a certificate a browser accepts
    curl -s -o /dev/null -w '%{http_code}' --max-time 10 --resolve "$1:443:$ADDRESS" "https://$1/" || true
}

on() {
    root
    local n line rules
    n=$(name)
    [ -s "$TLS/fullchain.pem" ] && covers "$TLS/fullchain.pem" "$n" || die "no certificate for $n in $TLS yet: cert.sh run"
    rules=$HERE/../../deploy/server/island.nft

    # The firewall first, live, and into its file only once it holds.
    if nft list table inet island 2>/dev/null | grep -q 'iifname "wg-island" tcp dport {[^}]*\b443\b'; then
        say "443 is open inside the tunnel"
    else
        grep -q 'iifname "wg-island" tcp dport { 22, 443, 4747, 4749 } accept' "$rules" \
            || die "$rules is not the file this script knows; load it by hand (docs/server.md)"
        nft -c -f "$rules" || die "$rules does not load; nothing changed"
        nft -f "$rules"
        nft list table inet island | grep -q 'iifname "wg-island" tcp dport {[^}]*\b22\b' \
            || die "the loaded firewall has no SSH rule on wg-island: do not close this session; nft -f $NFT brings the old one back"
        install -m 644 "$rules" "$NFT"
        say "opened 443 inside the tunnel (and wrote $NFT)"
    fi

    line="FLARE_TLS=--tls-listen $ADDRESS:443 --tls-cert $TLS/fullchain.pem --tls-key $TLS/privkey.pem --allow-host $n"
    if [ "$(cat "$FLARE_ENV" 2>/dev/null)" = "$line" ] && [ "$(serves "$n")" = 200 ]; then
        say "flare-server serves https://$n/ already"
    else
        echo "$line" > "$FLARE_ENV"
        chmod 644 "$FLARE_ENV"
        systemctl restart flare-server
        for _ in 1 2 3 4 5 6 7 8 9 10; do [ "$(serves "$n")" = 200 ] && break; sleep 1; done
        if [ "$(serves "$n")" != 200 ]; then
            mv -f "$FLARE_ENV" "$FLARE_ENV.failed"
            systemctl restart flare-server
            journalctl -u flare-server -n 15 --no-pager -o cat
            die "flare-server did not serve https://$n/ ($(serves "$n")); took the setting out again ($FLARE_ENV.failed) and restarted it as it was"
        fi
        say "flare-server serves https://$n/ (restarted; the plain page on 4749 is as before)"
    fi

    systemctl enable -q --now island-cert.timer
    say "renewal is on: $(systemctl show island-cert.timer -p NextElapseUSecRealtime --value)"
    say "the page: https://$n/   then: Island, This device"
}

status() {
    local n
    n=$(name)
    if [ -s "$TLS/fullchain.pem" ]; then
        echo "certificate  for $n: $(covers "$TLS/fullchain.pem" "$n" && echo yes || echo NO), from $(issuer "$TLS/fullchain.pem"), until $(ends "$TLS/fullchain.pem")"
    else
        echo "certificate  none in $TLS yet"
    fi
    echo "renewal      timer $(systemctl is-enabled island-cert.timer 2>/dev/null || echo off), next $(systemctl show island-cert.timer -p NextElapseUSecRealtime --value 2>/dev/null || true); last run: $(systemctl show island-cert@live.service -p Result --value 2>/dev/null || true)"
    echo "https        $(serves "$n") from https://$n/"
}

case ${1:-} in
    setup) setup "${2:-}" ;;
    units) root; units ;;
    token) token ;;
    dns) dns ;;
    try) try ;;
    run) run ;;
    on) on ;;
    status) status ;;
    verify) root; n=$(name); python3 "$HERE/desec.py" verify --name "$n" --env "$ENV" ;;
    issue) issue "${2:-}" ;;
    *) sed -n '2,/^$/s/^# \{0,1\}//p' "$0"; exit 2 ;;
esac
