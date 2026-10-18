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

This puts `wayshare` in `~/.local/bin` and sets up its systemd user service, without sudo.
Builds are for x86_64 and ARM64.

Other ways to install:

- With [mise](https://mise.jdx.dev): `mise use -g github:wesleygrimes/wayshare`, then
  `wayshare install-service`
- [From source](docs/development.md#installing-from-source)

wayshare isn't on the AUR or in `omarchy pkg` yet. Those packages are coming soon.

## Set up

1. Pick a password. Screen Sharing asks for this user name and password when you connect.

   ```sh
   wayshare set-password --user "$USER"
   ```

2. Tell wayshare which monitor to share. `hyprctl monitors` lists them; replace `DP-1` with
   yours.

   ```sh
   mkdir -p ~/.config/wayshare
   echo 'output = "DP-1"' > ~/.config/wayshare/config.toml
   ```

3. Start it with your session:

   ```sh
   systemctl --user enable --now wayshare
   ```

   On Omarchy that's all. Elsewhere, if wayshare doesn't come back after you log in again,
   add [one line to `hyprland.conf`](docs/troubleshooting.md#the-service-doesnt-start-at-login).

4. On the Mac, open the **Screen Sharing** app (search for it with Spotlight), choose
   **Connection > New** and enter the Linux machine's address, like `192.168.1.20`. Sign in
   with the password from step 1. To get sharper, low-latency video and sound, pick
   **High Performance** when it asks (see [High Performance mode](docs/high-performance.md)).

   The machine also shows up in Finder's sidebar under **Network**.

Connection trouble? See [Troubleshooting](docs/troubleshooting.md).

## Upgrade

Run the install command again. It restarts the service for you.

With mise or a package, upgrade the usual way, then restart the service:

```sh
systemctl --user restart wayshare
```

## Uninstall

```sh
systemctl --user disable --now wayshare
rm -f ~/.local/bin/wayshare ~/.local/share/systemd/user/wayshare.service
```

If you installed with mise or a package, remove it that way too. Your password and settings
stay in your config directory (usually `~/.config/wayshare`) until you delete it.

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
