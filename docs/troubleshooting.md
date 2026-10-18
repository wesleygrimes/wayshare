# Troubleshooting

Start with `wayshare doctor`. For anything it doesn't catch, read the log:

```sh
journalctl --user -u wayshare -f
```

## The service doesn't start at login

The service starts with `graphical-session.target`. Omarchy and other setups that launch
Hyprland through UWSM reach that target; a plain `Hyprland` launch doesn't. Start it from
`hyprland.conf` instead:

```
exec-once = systemctl --user start wayshare
```

## The Mac can't connect

Screen Sharing allows one window per address. Close any open window for this machine,
including ones behind other windows, and dismiss any leftover alerts.

## The password is rejected

Set it again and restart:

```sh
wayshare set-password
wayshare restart
```

## The machine doesn't show up in Finder's sidebar

mDNS needs UDP 5353 open, and `[mdns] enabled = true` (the default). You can always connect
by address instead.
