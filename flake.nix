{
  description = "NixOS development shell and package for Strata (fork's nix-dev branch)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
        # The package links UnRAR (unfree); nothing else here is allowed to be.
        config.allowUnfreePredicate = pkg: lib.getName pkg == "strata";
      };
      lib = pkgs.lib;

      miseConfig = builtins.fromTOML (builtins.readFile ./mise.toml);
      miseLock = builtins.fromTOML (builtins.readFile ./mise.lock);
      rustPin = miseConfig.tools.rust;

      rustMinimal = pkgs.rust-bin.stable.${rustPin.version}.minimal;

      rust = pkgs.rust-bin.stable.${rustPin.version}.default.override {
        extensions = lib.unique ((rustPin.components or [ ]) ++ [ "rust-src" ]);
      };

      # Prebuilt release binaries pinned (URL + checksum) by upstream's mise.lock.
      lockedTool =
        name: binary:
        let
          entry = builtins.head miseLock.tools.${name};
          platform = entry."platforms.linux-x64";
        in
        pkgs.stdenv.mkDerivation {
          pname = binary;
          inherit (entry) version;
          src =
            let
              checksum = lib.splitString ":" platform.checksum;
            in
            pkgs.fetchurl {
              inherit (platform) url;
              ${builtins.head checksum} = builtins.elemAt checksum 1;
            };
          sourceRoot = ".";
          nativeBuildInputs = [ pkgs.autoPatchelfHook ];
          buildInputs = [ pkgs.stdenv.cc.cc.lib ];
          installPhase = ''
            install -Dm755 "$(find . -type f -name ${binary} | head -n1)" "$out/bin/${binary}"
          '';
        };

      lockedTools = [
        (lockedTool "aqua:EmbarkStudios/cargo-deny" "cargo-deny")
        (lockedTool "aqua:crate-ci/typos" "typos")
        (lockedTool "aqua:watchexec/cargo-watch" "cargo-watch")
      ];

      # nixpkgs' mise lags behind mise.toml's min_version; the static release runs as-is.
      mise = pkgs.stdenvNoCC.mkDerivation rec {
        pname = "mise";
        version = "2026.9.17";
        src = pkgs.fetchurl {
          url = "https://github.com/jdx/mise/releases/download/v${version}/mise-v${version}-linux-x64-musl.tar.gz";
          hash = "sha256-rpXf+BVubDwdVcDoTNxaeYl7iYUSDnDt3YWbKbjXC48=";
        };
        installPhase = "install -Dm755 bin/mise $out/bin/mise";
      };

      python = pkgs.python314.withPackages (ps: [
        ps.pygobject3
        ps.pycairo
      ]);

      gst = pkgs.gst_all_1;

      runtimeLibraries = [
        pkgs.glib
        pkgs.gtk4
        pkgs.gtksourceview5
        pkgs.poppler
        pkgs.fontconfig
        pkgs.cairo
        pkgs.pango
        pkgs.gdk-pixbuf
        pkgs.graphene
        pkgs.librsvg
        pkgs.at-spi2-core
        gst.gstreamer
        gst.gst-plugins-base
        gst.gst-plugins-good
      ];

      dataPackages = [
        pkgs.gtk4
        pkgs.gtksourceview5
        pkgs.gsettings-desktop-schemas
        pkgs.adwaita-icon-theme
        pkgs.hicolor-icon-theme
        pkgs.shared-mime-info
        pkgs.at-spi2-core
      ];

      xdgDataDirs = lib.concatStringsSep ":" (
        map (p: "${p}/share/gsettings-schemas/${p.name}") [
          pkgs.gtk4
          pkgs.gsettings-desktop-schemas
          pkgs.gtksourceview5
        ]
        ++ map (p: "${p}/share") dataPackages
      );

      giTypelibPath = lib.makeSearchPathOutput "lib" "lib/girepository-1.0" [
        pkgs.glib
        pkgs.gtk4
        pkgs.pango
        pkgs.gdk-pixbuf
        pkgs.graphene
        pkgs.harfbuzz
        pkgs.at-spi2-core
        pkgs.gobject-introspection
      ];

      gstPluginPath = lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" [
        gst.gstreamer
        gst.gst-plugins-base
        gst.gst-plugins-good
      ];

      fontsConf = pkgs.makeFontsConf {
        fontDirectories = [
          pkgs.cantarell-fonts
          pkgs.dejavu_fonts
        ];
      };

      graphicalEnvironment = {
        XDG_DATA_DIRS = xdgDataDirs;
        GI_TYPELIB_PATH = giTypelibPath;
        GST_PLUGIN_SYSTEM_PATH_1_0 = gstPluginPath;
        GDK_PIXBUF_MODULE_FILE = "${pkgs.librsvg}/${pkgs.gdk-pixbuf.moduleDir}.cache";
        FONTCONFIG_FILE = "${fontsConf}";
        TZDIR = "${pkgs.tzdata}/share/zoneinfo";
      };

      # The build environment stdenv would give crates linking the GTK stack.
      buildEnvironment = pkgs.stdenv.mkDerivation {
        name = "strata-build-environment";
        dontUnpack = true;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = runtimeLibraries;
        installPhase = ''
          export -p | grep -E '^declare -x (PKG_CONFIG_PATH|NIX_CFLAGS_COMPILE|NIX_LDFLAGS)=' > $out
        '';
      };

      # NixOS without virtualisation.podman has no /etc/containers, and podman
      # reads a user policy only from $HOME/.config/containers.
      podmanPolicy = pkgs.writeText "policy.json" (
        builtins.toJSON {
          default = [ { type = "insecureAcceptAnything"; } ];
        }
      );

      # scripts/e2e.sh prefers podman, whose --userns=keep-id keeps the bind-mounted
      # checkout writable; rootless Docker maps the container user to a subuid.
      podman = pkgs.symlinkJoin {
        name = "podman-with-policy";
        paths = [ pkgs.podman ];
        nativeBuildInputs = [ pkgs.makeWrapper ];
        postBuild = ''
          wrapProgram $out/bin/podman \
            --run 'export XDG_DATA_HOME="''${XDG_DATA_HOME:-$HOME/.local/share}"' \
            --run 'HOME="''${XDG_CACHE_HOME:-$HOME/.cache}/strata-podman-home"' \
            --run 'install -Dm644 ${podmanPolicy} "$HOME/.config/containers/policy.json"' \
            --run 'export HOME'
        '';
      };

      # scripts/test-headless.py hands cargo only PATH and FHS XDG_DATA_DIRS, so
      # cargo carries its own build and GTK runtime environment. Always sourcing
      # it also keeps build-script fingerprints identical across callers.
      cargo = pkgs.symlinkJoin {
        name = "cargo-with-gtk-environment";
        paths = [ rust ];
        nativeBuildInputs = [ pkgs.makeWrapper ];
        postBuild = ''
          wrapProgram $out/bin/cargo \
            --run 'source ${buildEnvironment}' \
            --suffix XDG_DATA_DIRS : ${lib.escapeShellArg xdgDataDirs} ${
              lib.concatStringsSep " " (
                lib.mapAttrsToList (name: value: "--set-default ${name} ${lib.escapeShellArg value}") (
                  removeAttrs graphicalEnvironment [ "XDG_DATA_DIRS" ]
                )
              )
            }
        '';
      };

      cargoManifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      shortRev = self.shortRev or self.dirtyShortRev or "unknown";

      strata = pkgs.callPackage ./nix/package.nix {
        # rust-src would make std's panic locations point into the store.
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustMinimal;
          rustc = rustMinimal;
        };
        toolchain = rustMinimal;
        version = "${cargoManifest.package.version}+fork.${shortRev}";
        commit = self.rev or self.dirtyRev or "unknown";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.difference ./. (
            lib.fileset.unions (
              map lib.fileset.maybeMissing [
                ./.github
                ./.mise
                ./.release
                ./docs
                ./nix
                ./packaging
                ./flake.nix
                ./flake.lock
                ./NIX.md
              ]
            )
          );
        };
      };
    in
    {
      packages.${system} = {
        inherit strata;
        default = strata;
      };

      devShells.${system}.default = pkgs.mkShell {
        packages = [
          cargo
          mise
          python
          pkgs.pkg-config
          pkgs.glib.dev
          pkgs.gobject-introspection
          pkgs.git

          pkgs.bubblewrap
          pkgs.ffmpeg
          pkgs.ffmpegthumbnailer
          pkgs.util-linux

          pkgs.xorg-server
          pkgs.dbus
          pkgs.imagemagick
          podman
        ]
        ++ lockedTools;

        buildInputs = runtimeLibraries;

        env = graphicalEnvironment // {
          # mise runs tasks only; every tool comes from this shell.
          MISE_DISABLE_TOOLS = lib.concatStringsSep "," (builtins.attrNames miseConfig.tools);
          MISE_AUTO_INSTALL = "false";
          MISE_EXEC_AUTO_INSTALL = "false";
          MISE_TASK_RUN_AUTO_INSTALL = "false";
          MISE_NOT_FOUND_AUTO_INSTALL = "false";
          # .release/assemble.sh trains rerere from earlier release merges.
          GIT_RERERE_TRAIN = "${pkgs.git}/share/git/contrib/rerere-train.sh";
        };

        # The headless harness looks for AT-SPI daemons in FHS libexec paths, then PATH.
        shellHook = ''
          export PATH="$PATH:${pkgs.at-spi2-core}/libexec"
          if root="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
            export MISE_TRUSTED_CONFIG_PATHS="$(dirname "$root")"
          fi
        '';
      };
    };
}
