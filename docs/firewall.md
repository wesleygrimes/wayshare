# Firewall

| Port | Needed | Why |
|---|---|---|
| TCP 5900 | yes | the Screen Sharing connection |
| UDP 5353 | optional | mDNS, so the machine shows up in Finder's sidebar |
| UDP 5960, 5962 | no | High Performance video and audio. wayshare sends first, so replies come back through connection tracking |

`wayshare setup` and `wayshare doctor` print the ufw rule for your network when ufw is on.
To add the rules yourself for your local network (adjust the range to yours):

```sh
sudo ufw allow from 192.168.1.0/24 to any port 5900 proto tcp
sudo ufw allow from 192.168.1.0/24 to any port 5353 proto udp
```

If wayshare listens only on Tailscale, allow that interface instead:

```sh
sudo ufw allow in on tailscale0 to any port 5900 proto tcp
```

Don't open these ports to the internet. See [Security](security.md) for remote access.
