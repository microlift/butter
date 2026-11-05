{
  description = "Butter: a GTK 4 and libadwaita manager for Rosemary";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
    }:
    let
      # Butter is a GTK 4 application and only supports Linux.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

      # nixpkgs' rustc can lag behind the Rust version Butter (and the
      # Rosemary releases it builds) need, so use the latest stable.
      toolchainFor = pkgs: pkgs.rust-bin.stable.latest.default;

      appId = "io.github.microlift.butter";

      commonFor =
        pkgs:
        let
          craneLib = (crane.mkLib pkgs).overrideToolchain (toolchainFor pkgs);
          lib = pkgs.lib;

          # crane's default source filter drops everything but Rust files;
          # the desktop file, metainfo and icons are needed to install.
          src = lib.cleanSourceWith {
            src = ./.;
            filter = path: type: (craneLib.filterCargoSources path type) || (lib.hasInfix "/data/" path) || (lib.hasInfix "/plugins/" path) || (lib.hasInfix "/sdk/" path) || (baseNameOf path == "build.rs");
          };

          commonArgs = {
            inherit src;
            strictDeps = true;

            nativeBuildInputs = with pkgs; [
              pkg-config
              glib.dev # glib-compile-resources, for the bundled icons
              wrapGAppsHook4
              desktop-file-utils
              appstream
            ];

            buildInputs = with pkgs; [
              gtk4
              libadwaita
              glib
              gdk-pixbuf
              pango
              cairo
              graphene
              librsvg
              gsettings-desktop-schemas
              vte-gtk4
            ];
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        {
          inherit craneLib commonArgs cargoArtifacts;
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          inherit (commonFor pkgs) craneLib commonArgs cargoArtifacts;

          butter = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              pname = "butter";
              version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

              # Tests touch the user's data directories and the network.
              doCheck = false;

              postInstall = ''
                install -Dm644 data/${appId}.desktop $out/share/applications/${appId}.desktop
                install -Dm644 data/${appId}.metainfo.xml $out/share/metainfo/${appId}.metainfo.xml
                install -Dm644 data/icons/hicolor/scalable/apps/${appId}.svg \
                  $out/share/icons/hicolor/scalable/apps/${appId}.svg
                install -Dm644 data/icons/hicolor/symbolic/apps/${appId}-symbolic.svg \
                  $out/share/icons/hicolor/symbolic/apps/${appId}-symbolic.svg
              '';

              meta = {
                description = "Manage Rosemary runners and prefixes";
                homepage = "https://github.com/microlift/butter";
                license = pkgs.lib.licenses.gpl3Plus;
                mainProgram = "butter";
                platforms = systems;
              };
            }
          );
        in
        {
          inherit butter;
          default = butter;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.butter}/bin/butter";
        };
      });

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          inherit (commonFor pkgs) craneLib commonArgs cargoArtifacts;
        in
        {
          inherit (self.packages.${system}) butter;

          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );

          fmt = craneLib.cargoFmt { inherit (commonArgs) src; };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          toolchain = (toolchainFor pkgs).override {
            extensions = [
              "rust-src"
              "rust-analyzer"
              "clippy"
              "rustfmt"
            ];
          };
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.butter ];

            packages =
              [ toolchain ]
              ++ (with pkgs; [
                # Building Rosemary runners from source (the Runners page).
                clang
                lld
                sdl3
                libxkbcommon
                curl
                zlib
                gst_all_1.gstreamer
                webkitgtk_6_0
                # make check
                desktop-file-utils
                appstream
              ]);

            # GTK needs its schemas and icons at run time; without them
            # `cargo run` aborts or shows missing icons.
            shellHook = ''
              export XDG_DATA_DIRS="${pkgs.gtk4}/share/gsettings-schemas/${pkgs.gtk4.name}:${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.adwaita-icon-theme}/share:$XDG_DATA_DIRS"
              export GIO_EXTRA_MODULES="${pkgs.glib-networking}/lib/gio/modules''${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}"
            '';

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (
              with pkgs;
              [
                gtk4
                libadwaita
                vte-gtk4
                sdl3
                curl
                zlib
                libglvnd
                vulkan-loader
                libxkbcommon
                wayland
              ]
            );
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt-rfc-style);
    };
}
