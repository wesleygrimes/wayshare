# wayshare

Share your Hyprland desktop with a Mac, using the Screen Sharing app it already has.
wayshare speaks Apple's Screen Sharing protocol, so there's nothing to install on the Mac.

- The Mac's ⌘ key sends Super, so your Hyprland and Omarchy shortcuts work as usual.
- Copy and paste text, rich text and images in both directions.
- Screen Sharing's High Performance mode works, for sharp, low-latency video and sound.

![An Omarchy desktop shared to a Mac with Screen Sharing](docs/media/screenshot.jpg)

Needs Hyprland 0.56 or newer and systemd.

## Demo

![wayshare demo](https://github.com/user-attachments/assets/b43a3ae0-370d-4816-958b-87a16504bd8a)

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/wesleygrimes/wayshare/main/install.sh | sh
```

This puts `wayshare` in `~/.local/bin`, without sudo.
Builds are for x86_64 and ARM64.

Other ways to install:

- With [mise](https://mise.jdx.dev): `mise use -g github:wesleygrimes/wayshare`
- [From source](docs/development.md#installing-from-source)

## Set up

Run this in a terminal on your Hyprland desktop:

```sh
wayshare setup
```

It asks for a password, which monitor to share and which network to listen on. Then it
starts the service and tells you what to type on the Mac. If ufw is blocking the port, it
offers to add a rule for you, using sudo. Not on Omarchy? If wayshare doesn't come back after you log in
again, add [one line to `hyprland.conf`](docs/troubleshooting.md#the-service-doesnt-start-at-login).

On the Mac, open the **Screen Sharing** app (search for it with Spotlight), choose
**Connection > New** and enter the address setup printed. Sign in with your Linux user name
and the password you picked. For sharper, low-latency video and sound, pick **High
Performance** when it asks (see [High Performance mode](docs/high-performance.md)).

The machine also shows up in Finder's sidebar under **Network**.

Something not working? Run `wayshare doctor`. It checks each piece and tells you how to fix
what's missing. [Troubleshooting](docs/troubleshooting.md) covers the rest.

## Upgrade

```sh
wayshare upgrade
```

This installs the latest release and restarts the service. If you installed with mise, it
tells you the mise command instead. Coming from 0.1.5 or older, which has no `upgrade`, run
the install command again, then `wayshare restart`.

## Start, stop and restart

`wayshare start`, `wayshare stop` and `wayshare restart` control the service. Restart after
changing the config.

## Uninstall

```sh
wayshare uninstall
```

This stops the service and removes wayshare. With mise, run it before `mise uninstall`, since
mise can't stop the service for you. Your password and settings stay in `~/.config/wayshare`
until you delete it.

## More

- [Configuration](docs/configuration.md): every setting, keyboard mapping, the sidebar name
- [High Performance mode](docs/high-performance.md): what it needs, encoders, audio
- [Firewall](docs/firewall.md): which ports to open
- [Security](docs/security.md): what's encrypted, and using Tailscale or a VPN
- [Troubleshooting](docs/troubleshooting.md): logs and the usual problems
- [Packaging](docs/packaging.md): install layout and dependencies, for distro packagers
- [Development](docs/development.md): building, tests, live checks

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
