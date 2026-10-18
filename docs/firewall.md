# Firewall

| Port | Needed | Why |
|---|---|---|
| TCP 5900 | yes | the Screen Sharing connection |
| UDP 5353 | optional | mDNS, so the machine shows up in Finder's sidebar |
| UDP 5960, 5962 | no | High Performance video and audio. wayshare sends first, so replies come back through connection tracking |

With ufw, allowing your local network (adjust the range to yours):

```sh
sudo ufw allow from 192.168.1.0/24 to any port 5900 proto tcp
sudo ufw allow from 192.168.1.0/24 to any port 5353 proto udp
```

Don't open these ports to the internet. See [Security](security.md) for remote access.
