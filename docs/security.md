# Security

wayshare is meant for your home or office network. To reach your machine from anywhere
else, connect over [Tailscale](https://tailscale.com) or another VPN. Don't forward port
5900 on your router to the internet.

## What's encrypted

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

Set `listen` to the machine's Tailscale address (`tailscale ip -4` prints it) in
`~/.config/wayshare/config.toml`:

```toml
listen = "100.101.102.103:5900"
```

wayshare then refuses connections from your local network and only answers over Tailscale.
