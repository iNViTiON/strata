# Strata built from this tree, for the fork's `release` branch.
#
# Based on Th1nkK1D's package in github:Th1nkK1D/nixos-config (pkgs/strata/package.nix):
#
#   MIT License
#
#   Copyright (c) 2026 Withee Poositasai
#
#   Permission is hereby granted, free of charge, to any person obtaining a copy
#   of this software and associated documentation files (the "Software"), to deal
#   in the Software without restriction, including without limitation the rights
#   to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
#   copies of the Software, and to permit persons to whom the Software is
#   furnished to do so, subject to the following conditions:
#
#   The above copyright notice and this permission notice shall be included in all
#   copies or substantial portions of the Software.
#
#   THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
#   IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
#   FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
#   AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
#   LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
#   OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
#   SOFTWARE.
#
# Changes from his package: builds the local tree with crane, the pinned
# toolchain and Cargo.lock (dependencies as their own cached derivation),
# points the sandbox at the store through upstream's build-time STRATA_SANDBOX_*
# variables, patches the call sites they do not cover, binds
# /run/opengl-driver for VA-API, and marks the bundled UnRAR license.
{
  lib,
  craneLib,
  toolchain,
  cargoVersion,
  src,
  version,
  commit,
  pkg-config,
  wrapGAppsHook4,
  bubblewrap,
  cairo,
  ffmpeg-headless,
  ffmpegthumbnailer,
  fontconfig,
  gdk-pixbuf,
  glib,
  gst_all_1,
  gtk4,
  gtksourceview5,
  imagemagick,
  libraw,
  pango,
  poppler,
  squashfsTools,
  util-linux,
  xdg-terminal-exec,
}:

let
  # Helpers run inside Bubblewrap with this PATH; upstream defaults to /usr/bin.
  sandboxPath = lib.makeBinPath [
    imagemagick
    libraw
    ffmpeg-headless
    ffmpegthumbnailer
    fontconfig
    squashfsTools
    util-linux
  ];
  prlimit = lib.getExe' util-linux "prlimit";
  # trusted_command also finds a system bwrap on NixOS; the store copy keeps
  # the package working without bubblewrap in environment.systemPackages.
  bwrap = ''Ok::<_, String>(std::path::PathBuf::from("${lib.getExe bubblewrap}"))'';

  buildInputs = [
    cairo
    fontconfig
    gdk-pixbuf
    glib
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gtk4
    gtksourceview5
    pango
    poppler
  ];

  # Every dependency, compiled once per Cargo.lock and toolchain: crane builds
  # it from a stub of the sources, so application changes reuse it. The version
  # stays the Cargo one, not the release's, for the same reason.
  cargoArtifacts = craneLib.buildDepsOnly {
    pname = "strata";
    version = cargoVersion;
    inherit src buildInputs;
    strictDeps = true;
    nativeBuildInputs = [ pkg-config ];
    doCheck = false;
  };
in
craneLib.buildPackage {
  pname = "strata";
  inherit version src cargoArtifacts;
  strictDeps = true;

  # The sandbox is written for an FHS host. Upstream reads its runtime paths at
  # build time (docs/preview-sandbox.md, "Packaging non-FHS runtimes"): bind the
  # store instead of /usr, give helpers a store PATH and the gdk-pixbuf loader
  # cache (bwrap clears the environment), and pin prlimit.
  env = {
    STRATA_BUILD_COMMIT = commit;
    STRATA_SANDBOX_PATH = sandboxPath;
    STRATA_SANDBOX_ROOT = "/nix/store";
    STRATA_SANDBOX_PRLIMIT = prlimit;
    STRATA_SANDBOX_GDK_PIXBUF_MODULE_FILE = "${gdk-pixbuf}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache";
  };

  # What those variables do not cover: pin bwrap, and expose /run/opengl-driver,
  # where libva loads its VA-API driver from and the sandbox does not otherwise
  # look.
  postPatch = ''
    substituteInPlace src/sandbox.rs \
      --replace-fail '"/app",' '"/app", "--ro-bind-try", "/run/opengl-driver", "/run/opengl-driver",' \
      --replace-fail 'crate::trusted_command::resolve("bwrap")' '${bwrap}'
    substituteInPlace src/sandbox/archive.rs src/sandbox/media.rs src/sandbox/browser.rs \
      --replace-fail 'crate::trusted_command::resolve("bwrap")' '${bwrap}'

    # --replace-fail misses call sites added upstream later, and a hardcoded
    # path bypasses the build-time variables.
    if grep -rnE --include='*.rs' --exclude=tests.rs --exclude-dir=tests \
      '\.arg\("/usr/bin/prlimit"\)|resolve\("bwrap"\)|"/usr/bin",|"/usr",' src/sandbox.rs src/sandbox; then
      echo "error: unpatched FHS sandbox paths remain; extend postPatch" >&2
      exit 1
    fi
  '';

  nativeBuildInputs = [
    pkg-config
    glib
    wrapGAppsHook4
  ];

  inherit buildInputs;

  # The suite drives real GTK widgets and Bubblewrap, which the build sandbox lacks.
  doCheck = false;

  disallowedReferences = [ toolchain ];

  passthru = { inherit cargoArtifacts; };

  postInstall = ''
    install -Dm644 data/io.github.lgse.Strata.desktop \
      $out/share/applications/io.github.lgse.Strata.desktop
    install -Dm644 data/icons/scalable/apps/io.github.lgse.Strata.svg \
      $out/share/icons/hicolor/scalable/apps/io.github.lgse.Strata.svg

    install -Dm644 data/io.github.lgse.Strata.FileManager1.service \
      $out/share/dbus-1/services/io.github.lgse.Strata.FileManager1.service
    substituteInPlace $out/share/dbus-1/services/io.github.lgse.Strata.FileManager1.service \
      --replace-fail /usr/bin/strata $out/bin/strata

    # Upstream installs the FileChooser portal per user from the app; on NixOS
    # it belongs in the package so xdg.portal.extraPortals can pick it up.
    install -Dm644 data/portal/strata.portal \
      $out/share/xdg-desktop-portal/portals/strata.portal
    install -Dm644 data/portal/org.freedesktop.impl.portal.desktop.strata.service.in \
      $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.strata.service
    substituteInPlace $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.strata.service \
      --replace-fail @STRATA_EXECUTABLE@ $out/bin/strata

    # Stops the in-app updater from offering to overwrite the store path.
    install -Dm644 /dev/stdin $out/share/strata/install-source.toml <<EOF
    manager = "Nix"
    update_command = "nixos-rebuild switch"
    EOF
  '';

  # "Open terminal here" runs xdg-terminal-exec; GStreamer plays the helper's
  # raw audio frames.
  preFixup = ''
    gappsWrapperArgs+=(
      --prefix PATH : "${lib.makeBinPath [ xdg-terminal-exec ]}"
      --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "${
        lib.makeSearchPath "lib/gstreamer-1.0" (
          with gst_all_1;
          [
            gstreamer
            gst-plugins-base
            gst-plugins-good
            gst-libav
          ]
        )
      }"
    )
  '';

  meta = {
    description = "Fast, keyboard-first file manager for modern Linux desktops (iNViTiON fork release)";
    homepage = "https://github.com/iNViTiON/strata";
    # unrar_sys statically links UnRAR, whose license is not free.
    license = [
      lib.licenses.mit
      lib.licenses.unfreeRedistributable
    ];
    mainProgram = "strata";
    platforms = lib.platforms.linux;
  };
}
