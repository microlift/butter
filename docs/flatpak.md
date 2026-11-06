# Flatpak

Releases include a ready-made bundle, built by
[`.github/workflows/release.yml`](../.github/workflows/release.yml) when a
`v*` tag is pushed. The tag must match the version in `Cargo.toml`. The
workflow caches the Flatpak runtimes and `flatpak-builder` state between
runs. You can also run it by hand from the Actions tab; that uploads the
bundle as a workflow artifact without publishing a release.

To build it yourself:

```sh
flatpak install flathub org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08
flatpak-builder --user --install --force-clean build-dir build-aux/io.github.microlift.butter.yml
flatpak run io.github.microlift.butter
```

The Flatpak uses the GNOME **SDK** as its runtime, the way GNOME Builder
does, so that runners can be compiled inside the sandbox. SDL 3.4.0, which
Rosemary needs, is built into the Flatpak. Rust is not taken from the SDK
extension, which can be older than Rosemary needs. Instead, choose
**Runners -> Rust Toolchain -> Install**, and Butter installs a private
toolchain with rustup.

The Flatpak asks for as little access as it can. It reaches your files
through the file chooser. To run programs from a folder without choosing
them each time, allow it:

```sh
flatpak override --user --filesystem=~/Games io.github.microlift.butter
```

Some optional features need more. When Butter first starts, it asks which
ones you want, then shows the commented `flatpak override` command that
allows exactly what they need. Run it in a terminal and restart Butter.

The Flatpak includes [roserpc](https://github.com/st1rynight/roserpc), which
the Roblox plugin starts with Roblox to show the game on Discord. For that it
can reach Discord's IPC socket (`xdg-run/discord-ipc-0`, and the folders the
Flatpak Discord and Vesktop keep theirs in) without an override.

Shortcuts are made through the Dynamic Launcher portal, which needs no
extra permission. A shortcut can only open the files Butter can reach, so to
open documents from your file manager with a bottle's application, let
Butter see the folders they are in, as above.

Butter reads what the sandbox allows from `/.flatpak-info` at each launch:

- If you chose a feature but the command never ran, Butter reminds you.
- If a permission is taken away later, Butter tells you once which features
  stopped working.
- Switching on a feature that lacks its permission, such as a bottle's
  **Steam Integration**, shows the command for it.
- **Preferences -> Sandbox Permissions** lists each feature, with commands
  to allow it or remove it again, and can run the onboarding again.

Permission preferences are kept in `$XDG_CONFIG_HOME/butter/permissions.toml`, one
entry per onboarding step, with the permissions that step asked for. If a
later version needs different permissions for a feature, Butter asks about
it again and offers to remove what is no longer needed.
