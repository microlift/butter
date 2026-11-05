# Where things are

| What | Where |
| --- | --- |
| Runners | `$XDG_DATA_HOME/butter/runners/<id>/` (`runner.toml`, `bin/rosemary`, and `Rosemary/` for an official build) |
| Bottles | `$XDG_DATA_HOME/butter/bottles/<id>/` (`bottle.toml`, `prefix/`, `logs/`, and `run/` while programs run) |
| Butter's Rust toolchain | `$XDG_DATA_HOME/butter/toolchain/` |
| Runner builds in progress, resumed on the next launch if Butter quits first | `$XDG_DATA_HOME/butter/builds.toml` |
| Build cache and release list | `$XDG_CACHE_HOME/butter/` |
| Application icons, converted to PNG | `$XDG_CACHE_HOME/butter/icons/` |
| Shortcuts | Installed by the Dynamic Launcher portal, as `io.github.microlift.butter.<bottle>.app_<hash>.desktop`. Without the portal (outside the Flatpak only), in `$XDG_DATA_HOME/applications/`, with icons in `$XDG_DATA_HOME/butter/shortcuts/` |
| Preferences | `$XDG_CONFIG_HOME/butter/settings.toml` |
| Onboarding answers (Flatpak) | `$XDG_CONFIG_HOME/butter/permissions.toml` |

In the Flatpak, these are under `~/.var/app/io.github.microlift.butter/`.

A bottle's `prefix/` directory is an ordinary Rosemary prefix. Butter runs
programs with `ROSEMARY_PREFIX` pointing at it, and manages it with
`rosemary prefix create|info|verify|repair|reset-system|update`. Its settings
become Rosemary's environment variables, such as `ROSEMARY_DECORATIONS`,
`ROSEMARY_MENU`, `ROSEMARY_SWAP_INTERVAL`, `ROSEMARY_METAL_PRESENTATION_MODE`,
`ROSEMARY_SCREEN_REFRESH_RATE` and `ROSEMARY_PLAIN_CTRL_CLICK`. See Rosemary's
[usage guide](https://github.com/microlift/rosemary/blob/main/docs/usage.md).

## Environment variables

Rosemary keeps the programs' environment apart from its own: a Mac program
starts with only `HOME`, `USER`, `LOGNAME`, `SHELL`, `PATH`, `TMPDIR` and
`PWD`, the host's `TERM`, `COLORTERM`, `LANG` and `LC_*`, and whatever
`ROSEMARY_ENVIRON` lists. So each variable on a bottle's page is set for one
of:

- **Programs**: a line of `ROSEMARY_ENVIRON`. With **Use This Computer's
  Value**, the line is the bare name, which copies Butter's own value of it.
- **Rosemary**: Rosemary's own environment, where its settings and those of
  the host libraries it loads (Vulkan, Mesa, SDL) are read. Programs never see
  these. They override the bottle's settings.
- **Programs and Rosemary**: both.

A variable can be switched off without removing it. **Passed On to Launched
Programs** becomes `ROSEMARY_FORWARD_ENV_VARS`: the variables a program may set
for another Mac program it starts.

Bottles made by an older Butter kept their variables in an `[env]` table,
which Rosemary's programs used to see along with the rest of its environment.
Each is moved to the new list when the bottle is loaded: names beginning with
`ROSEMARY_` are set for Rosemary, the rest for both, so programs keep seeing
what they saw before.

## GPUs

The **Metal GPU** and **OpenGL GPU** settings list the PCI graphics cards in
`/sys/class/drm`. **Automatic** sets nothing. Choosing a card sets, for
Rosemary:

| Setting | Card's driver | Variables |
| --- | --- | --- |
| Metal GPU | any | `MESA_VK_DEVICE_SELECT=<vendor>:<device>`, `MESA_VK_DEVICE_SELECT_FORCE_DEFAULT_DEVICE=1`, and `__VK_LAYER_NV_optimus=NVIDIA_only` (`nvidia`) or `non_NVIDIA_only` (others) |
| OpenGL GPU | `nvidia` | `__NV_PRIME_RENDER_OFFLOAD=1`, `__GLX_VENDOR_LIBRARY_NAME=nvidia` |
| OpenGL GPU | others (Mesa) | `DRI_PRIME=pci-<domain>_<bus>_<slot>_<function>` |

The Metal GPU depends on Mesa's Vulkan device selection layer, which Mesa
installs on most systems and the Flatpak runtime includes. The Metal GPU is
ignored while **Disable Metal Support** is on.
