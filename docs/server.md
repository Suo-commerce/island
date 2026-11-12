# The Island server

How the broker and the shared Flare session move from the laptop to the
Island server, and how the server is closed to everyone but your own
devices. Creating the server itself is `deploy/server/create.sh`.

## What protects what

Reaching the broker gives no power to write. Every write is a submission
signed by a key that holds a grant, judged by the gate; every replica
verifies the whole log from genesis against the pinned root key, so an
entry that is changed, dropped or reordered is refused. The network is not
what keeps the log honest.

What reaching the broker does give is reading: reads are open (the spec's
choice for slice 1), so whoever reaches the port reads the whole log. And
whoever reaches it can load it with requests. The layers below are for
those two:

1. **A private network of your own devices.** The server and your devices
   are WireGuard peers on `10.77.0.0/24`, interface `wg-island`. The
   server accepts exactly the device keys listed in its configuration,
   and answers nothing that is not signed by one of them. Each device's
   private key is made on that device and never leaves it.
2. **The programs listen on the private address only.** The broker and
   `flare-server` listen on `10.77.0.1`, and both refuse to listen on
   every interface at once.
3. **The firewall.** `deploy/server/island.nft` drops everything coming
   in except WireGuard's UDP port; SSH, the broker and the session server
   answer only inside the tunnel. If the tunnel is down, they are closed.

The MCP helper is different: it holds a writer key and has no password.
It stays on the machine the coding sessions run on, on loopback, and never
moves to the server.

| Address | Device |
| --- | --- |
| `10.77.0.1` | the server, WireGuard on UDP 51820 |
| `10.77.0.2` | the laptop |
| `10.77.0.3` | the phone |

WireGuard runs beside Tailscale on the laptop without touching it: its own
interface, its own subnet, its own port. The laptop's tunnel carries only
`10.77.0.1`, nothing else. A phone runs one VPN at a time.

## 1. WireGuard on the server

On the server, as root, right after `create.sh` has made it. The private
key is made here and stays here:

```sh
apt-get update && apt-get install -y wireguard nftables
umask 077
wg genkey > /etc/wireguard/wg-island.key
wg pubkey < /etc/wireguard/wg-island.key   # the server's public key: note it
```

On the laptop, the same for the laptop's key:

```sh
sudo apt-get install -y wireguard
sudo sh -c 'umask 077; wg genkey > /etc/wireguard/wg-island.key'
sudo wg pubkey < /etc/wireguard/wg-island.key   # the laptop's public key: note it
```

Back on the server, with the laptop's public key:

```sh
read -rp "laptop public key: " LAPTOP
cat > /etc/wireguard/wg-island.conf <<END
[Interface]
Address = 10.77.0.1/24
ListenPort = 51820
PostUp = wg set %i private-key /etc/wireguard/wg-island.key

# laptop
[Peer]
PublicKey = $LAPTOP
AllowedIPs = 10.77.0.2/32
END
chmod 600 /etc/wireguard/wg-island.conf
systemctl enable --now wg-quick@wg-island
```

On the laptop, with the server's public key and its public address:

```sh
read -rp "server public key: " SERVER
read -rp "server public address: " ENDPOINT
sudo sh -c "umask 077; cat > /etc/wireguard/wg-island.conf" <<END
[Interface]
Address = 10.77.0.2/32
PostUp = wg set %i private-key /etc/wireguard/wg-island.key

[Peer]
PublicKey = $SERVER
Endpoint = $ENDPOINT:51820
AllowedIPs = 10.77.0.1/32
PersistentKeepalive = 25
END
sudo systemctl enable --now wg-quick@wg-island
ping -c 3 10.77.0.1
```

## 2. The firewall, then SSH inside the tunnel

First check that SSH works through the tunnel, from the laptop:

```sh
ssh root@10.77.0.1 true && echo "ssh over WireGuard works"
```

Copy the deploy files over:

```sh
scp -r deploy/server root@10.77.0.1:
```

Only then, on the server, close everything else. From here SSH answers
only at `10.77.0.1`. If the tunnel ever fails, UpCloud's web console
still reaches the server.

```sh
install -d /etc/nftables.d
install -m 644 ~/server/island.nft /etc/nftables.d/island.nft
grep -q nftables.d /etc/nftables.conf || echo 'include "/etc/nftables.d/*.nft"' >> /etc/nftables.conf
systemctl enable nftables
nft -f /etc/nftables.d/island.nft
nft list table inet island
```

## 3. The programs

On the laptop, build them and copy them over:

```sh
cd /srv/oc-projects/island
just build
scp target/release/island-broker target/release/island-cli target/release/flare-server root@10.77.0.1:/tmp/
ssh root@10.77.0.1 'install -m 755 /tmp/island-broker /tmp/island-cli /tmp/flare-server /usr/local/bin/'
```

`install -m 755` sets the mode the `island` user needs to run them; a copy
made with `scp` straight into place keeps whatever the remote umask leaves.

On the server, the `island` user and its settings. The root public key is
the same 64 hex digits as everywhere else; on the laptop,
`island-cli public .island/root.key` prints it. The root key never comes to
the server; the appender key is the one key the server holds.

```sh
read -rp "root public key: " ROOT
useradd --system --home-dir /var/lib/island --create-home --shell /usr/sbin/nologin island
install -d -m 755 /etc/island
sh -c "umask 077; cat > /etc/island/island.env" <<END
ISLAND_LOG=/var/lib/island/island.log
ISLAND_APPENDER_KEY=/var/lib/island/appender.key
ISLAND_ROOT=$ROOT
END
install -m 644 ~/server/island-broker.service ~/server/flare-server.service /etc/systemd/system/
systemctl daemon-reload
```

## 4. Moving the log

The log moves; it is not copied. One broker at a time holds it, and a log
written on two machines would fork. On the laptop, stop the local broker
and session server for good, copy the log and the appender key over, and
retire the laptop's copy so nothing can open it again:

```sh
cd /srv/oc-projects/island
systemctl --user disable --now island-broker flare-server
scp .island/island.log .island/appender.key root@10.77.0.1:/var/lib/island/
mv .island/island.log .island/island.log.moved-to-server
```

On the server:

```sh
chown island:island /var/lib/island/island.log /var/lib/island/appender.key
chmod 600 /var/lib/island/appender.key
systemctl enable --now island-broker flare-server
curl -s http://10.77.0.1:4747/head
```

The head is the laptop's last entry, with the same hash. The laptop's
replica keeps pulling from the server and finds the same history; if the
server ever showed another one, the replica would refuse it as a fork.

## 5. The laptop and the phone

On the laptop, the helper and the replica point at the server, and Flare
attaches there. In `~/.config/island/island.env`, set:

```sh
ISLAND_BROKER=http://10.77.0.1:4747
ISLAND_SESSION=ws://10.77.0.1:4749/session
```

then `systemctl --user restart island-helper flare-link`. `islander.sh`
(in `scripts/`; copy it to your home directory again) reads both, starts
only the helper and the replica, and opens Flare on the server.

For the phone, make the tunnel in the WireGuard app, so its private key is
made on the phone: address `10.77.0.3/32`, peer the server's public key,
endpoint the server's public address and port 51820, allowed IPs
`10.77.0.1/32`. Add the phone's public key on the server:

```sh
read -rp "phone public key: " PHONE
cat >> /etc/wireguard/wg-island.conf <<END

# phone
[Peer]
PublicKey = $PHONE
AllowedIPs = 10.77.0.3/32
END
wg set wg-island peer "$PHONE" allowed-ips 10.77.0.3/32
```

To take a device out, delete its `[Peer]` block and run
`wg set wg-island peer KEY remove`.

## Updating the server

`islander.sh update` updates only the laptop. After merging a bundle, the
server's programs go over the same way they first did:

```sh
cd /srv/oc-projects/island
just build
scp target/release/island-broker target/release/island-cli target/release/flare-server root@10.77.0.1:/tmp/
ssh root@10.77.0.1 'install -m 755 /tmp/island-broker /tmp/island-cli /tmp/flare-server /usr/local/bin/ && systemctl restart island-broker flare-server'
curl -s http://10.77.0.1:4747/head
```

The head names the build it runs.

## Later

If the log comes to hold what must stay private even from your own
devices, reads need authentication. That is a decision for the spec
before email and calendar come in.
