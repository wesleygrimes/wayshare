# wayshare

Share your Hyprland desktop with a Mac's Screen Sharing app or RDP clients like
Windows App and FreeRDP.

## Connect from

| Client | Quickstart |
|---|---|
| Mac Screen Sharing | [Connect from your Mac](docs/connect/mac.md) |
| Windows App on iPhone, iPad, or Mac | [Connect with Windows App](docs/connect/windows-app.md) |
| Windows Remote Desktop Connection (mstsc) | [Connect with mstsc](docs/connect/windows-app.md) |
| FreeRDP | [Connect with FreeRDP](docs/connect/freerdp.md) |

- Use your Hyprland and Omarchy shortcuts.
- On the Mac, copy text, rich text, and images. Use High Performance for video and sound.
- On RDP, use H.264 or Planar graphics. Live resize and rotation work on iPhone.

RDP is on by default at `0.0.0.0:3389`. See the [tested clients](docs/clients.md)
for each client's verified behavior.

<img src="docs/media/screenshot.jpg" alt="An Omarchy desktop open in the Mac's Screen Sharing app" width="720">

<details>
<summary>Watch the 40-second demo</summary>

https://github.com/user-attachments/assets/b43a3ae0-370d-4816-958b-87a16504bd8a

</details>

## Install

Install on your Hyprland machine. Requires Hyprland 0.56 or newer and systemd.
Builds are available for x86_64 and ARM64.

### Install on Omarchy

wayshare is in the Omarchy package repository:

```sh
omarchy pkg add wayshare
```

For now the package is only on Omarchy's edge channel. It reaches stable when Omarchy
promotes it. Until then, `pacman -Si wayshare` on stable says it isn't found.

### Install script

```sh
curl -fsSL https://raw.githubusercontent.com/wesleygrimes/wayshare/main/install.sh | sh
```

Installs to `~/.local/bin`. No sudo required.

### Install with mise

If you use [mise](https://mise.jdx.dev), run:

```sh
mise use -g github:wesleygrimes/wayshare
```

To build it yourself, see [Build from source](docs/development.md#install-from-source).

## Set up

Run this in a terminal inside your Hyprland session:

```sh
wayshare setup
```

Choose a password, the monitor to share, and the network for Apple Screen Sharing.
Setup saves your choices, starts wayshare, and displays the address and username.
Both protocols use that username and password. Setup offers ufw rules for TCP 5900
and 3389. On an existing install, rerun setup or open TCP 3389 to your LAN or VPN.

The network choice controls Apple Screen Sharing. RDP listens on all IPv4
interfaces by default. See [Security](docs/security.md) to restrict both protocols
to your VPN.

Run `wayshare setup` again to change your choices. For
[other settings](docs/configuration.md), edit `~/.config/wayshare/config.toml`, then run
`wayshare restart`.

## Connect

On a Mac, open Screen Sharing and connect to the address from setup. Sign in with
the username from setup and your wayshare password. Follow the
[Mac quickstart](docs/connect/mac.md) for High Performance mode.

In an RDP client, use your Hyprland machine's address and port 3389. Use the same
username, with an empty domain. Check the certificate's SHA-256 fingerprint before
you send your password. Follow the [Windows App and mstsc](docs/connect/windows-app.md)
or [FreeRDP](docs/connect/freerdp.md) quickstart.

For connection or startup problems, run `wayshare doctor` and see
[Troubleshooting](docs/troubleshooting.md). For RDP, doctor checks the ufw rule
but does not connect to the listener.

## Commands

| Command | What it does |
|---|---|
| `wayshare setup` | Set up or change the password, monitor, and network |
| `wayshare doctor` | Check the setup and explain how to fix problems |
| `wayshare start` | Start the service |
| `wayshare stop` | Stop the service |
| `wayshare restart` | Restart the service after configuration changes |

### Upgrade

```sh
wayshare upgrade
```

Installs the latest release and restarts the service.

For version 0.1.5 or older, run the install command again, then `wayshare restart`.

On Omarchy, `omarchy-update` upgrades wayshare with the rest of the system. Then run
`wayshare restart`.

### Uninstall

```sh
wayshare uninstall
```

Stops the service and removes wayshare. Keeps `~/.config/wayshare`.

With mise, run `wayshare uninstall` before `mise uninstall`.

On Omarchy, run `wayshare uninstall`, then `omarchy pkg drop wayshare`.

## Documentation

- [Client compatibility](docs/clients.md)
- [Configuration](docs/configuration.md)
- [High Performance mode](docs/high-performance.md)
- [RDP](docs/rdp.md)
- [Security](docs/security.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Packaging](docs/packaging.md)
- [Development](docs/development.md)

## Contributors

Thanks to these people ([emoji key](https://allcontributors.org/docs/en/emoji-key)):

<a id="maintenance-wesleygrimes"></a>
<a id="ideas-andresark"></a>
<a id="userTesting-kitknox"></a>
<a id="ideas-ernestodisa"></a>
<a id="userTesting-thebytorsnowdog"></a>
<a id="platform-3j27"></a>

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/wesleygrimes"><img src="https://avatars.githubusercontent.com/u/324308?v=4?s=100" width="100px;" alt="Wes Grimes"/><br /><sub><b>Wes Grimes</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/commits?author=wesleygrimes" title="Code">💻</a> <a href="https://github.com/wesleygrimes/wayshare/commits?author=wesleygrimes" title="Documentation">📖</a> <a href="#maintenance-wesleygrimes" title="Maintenance">🚧</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/andresark"><img src="https://avatars.githubusercontent.com/u/22714509?v=4?s=100" width="100px;" alt="Andre Alves"/><br /><sub><b>Andre Alves</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/issues?q=author%3Aandresark" title="Bug reports">🐛</a> <a href="#ideas-andresark" title="Ideas, Planning, & Feedback">🤔</a> <a href="https://github.com/wesleygrimes/wayshare/commits?author=andresark" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/kitknox"><img src="https://avatars.githubusercontent.com/u/41200331?v=4?s=100" width="100px;" alt="Kit Knox"/><br /><sub><b>Kit Knox</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/commits?author=kitknox" title="Code">💻</a> <a href="https://github.com/wesleygrimes/wayshare/issues?q=author%3Akitknox" title="Bug reports">🐛</a> <a href="#userTesting-kitknox" title="User Testing">📓</a></td>
      <td align="center" valign="top" width="14.28%"><a href="http://kylercao.com"><img src="https://avatars.githubusercontent.com/u/52361991?v=4?s=100" width="100px;" alt="kyler"/><br /><sub><b>kyler</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/commits?author=kyler505" title="Code">💻</a> <a href="https://github.com/wesleygrimes/wayshare/issues?q=author%3Akyler505" title="Bug reports">🐛</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/ernestodisa"><img src="https://avatars.githubusercontent.com/u/285854814?v=4?s=100" width="100px;" alt="ernestodisa"/><br /><sub><b>ernestodisa</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/issues?q=author%3Aernestodisa" title="Bug reports">🐛</a> <a href="#ideas-ernestodisa" title="Ideas, Planning, & Feedback">🤔</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/thebytorsnowdog"><img src="https://avatars.githubusercontent.com/u/215750096?v=4?s=100" width="100px;" alt="Snowy"/><br /><sub><b>Snowy</b></sub></a><br /><a href="#userTesting-thebytorsnowdog" title="User Testing">📓</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/bndnsmth"><img src="https://avatars.githubusercontent.com/u/4730164?v=4?s=100" width="100px;" alt="Brandon Smith"/><br /><sub><b>Brandon Smith</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/commits?author=bndnsmth" title="Code">💻</a></td>
    </tr>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/ChachizR"><img src="https://avatars.githubusercontent.com/u/51926047?v=4?s=100" width="100px;" alt="ChachizR"/><br /><sub><b>ChachizR</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/commits?author=ChachizR" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/3j27"><img src="https://avatars.githubusercontent.com/u/346161426?v=4?s=100" width="100px;" alt="3j27"/><br /><sub><b>3j27</b></sub></a><br /><a href="https://github.com/wesleygrimes/wayshare/commits?author=3j27" title="Code">💻</a> <a href="https://github.com/wesleygrimes/wayshare/issues?q=author%3A3j27" title="Bug reports">🐛</a> <a href="#platform-3j27" title="Packaging/porting to new platform">📦</a> <a href="https://github.com/wesleygrimes/wayshare/commits?author=3j27" title="Tests">⚠️</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

This project follows the [all-contributors](https://allcontributors.org) specification.
To contribute, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Support

wayshare is free. If it's useful, you can [sponsor the work](https://github.com/sponsors/wesleygrimes) or [buy me a coffee](https://buymeacoffee.com/ncfrontiersman).

## License

Apache-2.0. See [LICENSE](LICENSE).
