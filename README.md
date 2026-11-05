<img width="290" height="240" alt="butter dog" src="https://github.com/user-attachments/assets/8d789a20-f396-442d-9e48-d980e2fc775b" />

# Butter

Butter manages [Rosemary](https://github.com/microlift/rosemary), a
compatibility layer that runs macOS applications on Linux, the way
[Bottles](https://usebottles.com) manages Wine. It is a GTK 4 and libadwaita
application written in Rust.

- **Runners** are builds of Rosemary. Butter lists the tagged releases from
  Rosemary's GitHub repository. It installs the official build of the one you
  choose, or compiles it from source. You can also add a `rosemary` binary you
  built yourself.
- **Bottles** are Rosemary prefixes: each has its own Darwin filesystem (with
  the verified MiniDarwin base), its own runner, and its own settings and
  environment variables.
- **Applications** are listed on each bottle's page with their own names and
  icons: Butter finds every `.app` bundle installed in the bottle's
  `/Applications` (and each user's `~/Applications`) by itself. You can add
  more, such as a bundle elsewhere or a Mach-O executable, or give one
  arguments of its own. You can also run a program once without saving it.
- **Shortcuts** put an application in your desktop's applications menu. They
  also register it for the links and files it says it opens, so opening one
  of those on the desktop starts it in its bottle.
- The **Process Manager** lists everything running in a bottle. Each program
  gets a terminal of its own: you can read its output, type into it, copy
  and paste (right-click, or Ctrl+Shift+C and Ctrl+Shift+V), send it signals
  and stop it. A terminal can be popped out into its own window and back in.
  The terminal button opens a Rosemary shell (`rosemary run /bin/bash`) in the
  bottle. The power button in a bottle's header stops the whole bottle.

## Install

Butter supports three installations officially: Flatpak, Nix and building from
source.
Either can install official Rosemary builds, which need nothing beyond an
OpenGL or Vulkan driver, or you can compile runners on your computer.

### Flatpak

Each [release](https://github.com/microlift/butter/releases) has a
`Butter-<version>-x86_64.flatpak` bundle. Download it and install it:

```sh
flatpak install --user Butter-<version>-x86_64.flatpak
flatpak run io.github.microlift.butter
```

The bundle points at Flathub for the GNOME runtime it needs, so
[Flathub must be set up](https://flathub.org/setup) first. To check the
download, compare it with `SHA256SUMS` from the same release, or verify its
build provenance with `gh attestation verify <file> --repo microlift/butter`.

To build the Flatpak yourself instead:

```sh
flatpak install flathub org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08
flatpak-builder --user --install --force-clean build-dir build-aux/io.github.microlift.butter.yml
flatpak run io.github.microlift.butter
```

To let the Flatpak see a folder of programs, and for details on optional
permissions, see [Flatpak](docs/flatpak.md).

### Nix

The repository is a Nix flake for `x86_64-linux` and `aarch64-linux`:

```sh
nix run github:microlift/butter
```

To install it permanently, add it to a standard flake-based NixOS
configuration or to Home Manager. See [Installing with Nix](docs/building.md#installing-with-nix)
for both.

### From source

```sh
make
make install PREFIX=~/.local     # or: sudo make install
```

See [Building from source](docs/building.md) for dependencies, Nix and NixOS.

AppImage support is planned.

## Getting started

On first launch, Butter shows a short **Getting Started** tour: installing
runners from official builds or from source, creating a bottle, opening
terminals, finding logs and adding apps to the applications menu. Open it
again from the main menu or with F1.

1. Open **Runners** and install a Rosemary release.
2. Create a bottle and choose that runner.
3. Install an app into the bottle's `/Applications`, or add a `.app` bundle
   or Mach-O executable, and run it.
4. To start it from your desktop, choose **Create Shortcut** in its menu.

## Documentation

- [Flatpak](docs/flatpak.md): sandbox permissions and onboarding
- [Building from source](docs/building.md): dependencies and Nix
- [Where things are](docs/data-layout.md): runners, bottles, settings
- [How Butter works](docs/how-it-works.md): running programs, installing and building runners
- [Development](docs/development.md): running tests

## License

GPL-3.0-or-later
