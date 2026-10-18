# wayshare

Share your Hyprland desktop with your Mac.

Connect through the Mac's built-in Screen Sharing app. Nothing to install on macOS.

- Use your Hyprland and Omarchy shortcuts. Command sends Super.
- Copy and paste text, rich text, and images between machines.
- Use High Performance mode for low-latency video and sound.

<img src="docs/media/screenshot.jpg" alt="An Omarchy desktop open in the Mac's Screen Sharing app" width="720">

<details>
<summary>Watch the 40-second demo</summary>

https://github.com/user-attachments/assets/b43a3ae0-370d-4816-958b-87a16504bd8a

</details>

## Install

Install on your Hyprland machine. Requires Hyprland 0.56 or newer and systemd.
Builds are available for x86_64 and ARM64.

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

To build it yourself, see [Build from source](docs/development.md#installing-from-source).

## Configure

Run this in a terminal inside your Hyprland session:

```sh
wayshare setup
```

Choose a password, the monitor to share, and the network to accept connections on.
Setup saves your choices, starts wayshare, and displays the address and username for
your Mac.

If ufw blocks the connection, setup offers to add a firewall rule using sudo.

Run `wayshare setup` again to change these choices. For
[other settings](docs/configuration.md), edit `~/.config/wayshare/config.toml`, then run:

```sh
wayshare restart
```

## Connect from your Mac

1. Open **Screen Sharing**.
2. Choose **Connection → New** and enter the address shown by setup.
3. Sign in with the username shown by setup and the password you chose.
4. Select **High Performance** when prompted.

For connection or startup problems, run `wayshare doctor` and see
[Troubleshooting](docs/troubleshooting.md).

## Documentation

- [Configuration](docs/configuration.md): display, keyboard, network, and other settings
- [High Performance mode](docs/high-performance.md): encoders and audio requirements
- [Security](docs/security.md): encryption, ports, and VPN access
- [Troubleshooting](docs/troubleshooting.md): connection problems, startup, and logs
- [Packaging](docs/packaging.md): install layout and dependencies
- [Development](docs/development.md): building and testing

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

### Uninstall

```sh
wayshare uninstall
```

Stops the service and removes wayshare. Keeps `~/.config/wayshare`.

With mise, run `wayshare uninstall` before `mise uninstall`.

## Contributors

Thanks to these people ([emoji key](https://allcontributors.org/docs/en/emoji-key)):

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
