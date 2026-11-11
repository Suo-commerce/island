# The broker on the Island server

The server is not deployed yet. This is what its broker needs when it is,
so that reaching the broker is limited three times over: by the broker
itself, by the server's firewall, and by the tailnet's policy.

## What protects what

Reaching the broker gives no power to write. Every write is a submission
signed by a key that holds a grant, judged by the gate; every replica
verifies the whole log from genesis against the pinned root key, so an
entry that is changed, dropped or reordered is refused. Tailscale is not
what keeps the log honest.

What reaching the broker does give is reading: reads are open (the spec's
choice for slice 1), so whoever reaches the port reads the whole log. And
whoever reaches it can load it with requests. Those two are what the
layers below are for.

The MCP helper is different: it holds a writer key and has no password.
It runs on the machine the coding sessions run on, on loopback, never on
the tailnet. Both the broker and the helper refuse to listen on every
interface at once.

## 1. The broker listens on the tailnet address only

`deploy/server/island-broker.service`, with `/etc/island/island.env`:

The root public key is the same 64 hex digits as everywhere else; on the
laptop, `island-cli public` with the root key file prints it. The block
asks for it:

```sh
read -rp "root public key: " ROOT
sudo useradd --system --home-dir /var/lib/island --create-home --shell /usr/sbin/nologin island
sudo install -d -m 755 /etc/island
sudo sh -c "umask 077; cat > /etc/island/island.env" <<END
ISLAND_LOG=/var/lib/island/island.log
ISLAND_APPENDER_KEY=/var/lib/island/appender.key
ISLAND_ROOT=$ROOT
ISLAND_LISTEN=$(tailscale ip -4):4747
ISLAND_HOST=$(tailscale status --json | jq -r .Self.DNSName | sed 's/\.$//')
END
sudo install -m 644 deploy/server/island-broker.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now island-broker
```

The root key never comes to the server; only its public half goes into
`ISLAND_ROOT`. The appender key is the one key the server holds. If the
server is taken over, an attacker can seal new entries, but each still
needs a writer's signature and a grant, and a rewritten history shows up
as a fork in every replica.

## 2. The firewall closes the port on every other interface

`deploy/server/island.nft` drops the broker's port everywhere but `lo` and
`tailscale0`. If Tailscale is down or misconfigured, the port is still
closed.

## 3. The tailnet policy lets only your devices reach the port

`deploy/server/tailnet-policy.hujson` holds the grant: devices of the
tailnet's admins may reach `tag:island` on TCP 4747. Tag the server
`tag:island` and merge the grant into the policy file in the admin
console. Grants only allow, so a policy that still allows everything to
everything restricts nothing until that rule is removed.

## Later

If the log comes to hold what must stay private even from your own
devices, reads need authentication. That is a decision for the spec
before email and calendar come in.
