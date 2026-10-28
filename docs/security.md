# Security

wayshare is meant for your home or office network. To reach your machine from anywhere
else, connect over [Tailscale](https://tailscale.com) or another VPN. Don't forward port
5900 or 3389 on your router to the internet.

## What's encrypted

Apple Screen Sharing uses the encryption below. RDP encrypts credentials, input and
screen updates with TLS. Check its certificate fingerprint before sending a password.

| Traffic | Encrypted |
|---|---|
| Password | never sent; the Mac proves it knows it (SRP) |
| Keyboard and mouse | yes, by the Mac |
| Screen in Standard mode | **no** |
| Clipboard | **no** |
| Video and sound in High Performance mode | yes (SRTP) |

Anyone who can watch your network can see your screen in Standard mode and anything you
copy and paste. A VPN encrypts all of it.

## Pick a separate password

`wayshare set-password` stores a salted verifier, not the password. Someone who copies the
`srp-NAME` file can still guess passwords against it offline, so don't reuse your Linux
password or any other important one.

## Only accept connections over Tailscale

Run `wayshare setup` and pick `tailscale0` as the network for Apple RFB. Also set
`[rdp] listen = "100.x.y.z:3389"` to your Tailscale address, or set
`[rdp] enabled = false`. RDP defaults to all IPv4 interfaces independently of RFB.
Tailscale has to be up before wayshare starts. If it isn't, wayshare can't listen on the
address and the service fails; run `wayshare restart` once Tailscale is up.

## Ports

| Port | Needed | Why |
|---|---|---|
| TCP 5900 | yes | the Screen Sharing connection |
| TCP 3389 | when RDP is enabled, the default | TLS RDP with the shared SRP credentials |
| UDP 5353 | optional | mDNS, so the machine shows up in Finder's sidebar |
| UDP 5960, 5962 | no | High Performance video and audio. wayshare sends first, so replies come back through connection tracking |

`wayshare setup` offers to open TCP 5900 and the enabled RDP port in ufw, normally
TCP 3389. Existing setups must rerun setup or add the RDP rule themselves. With
another firewall, open the ports yourself, only to your local network or VPN.
RDP stays off if its SRP record or TLS identity is unavailable. Apple RFB keeps
listening and still requires credentials.
