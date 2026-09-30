# Strata (iNViTiON fork)

Fork of [lgse/strata](https://github.com/lgse/strata). For what Strata is, how
to install and use it, and its documentation, read the
[upstream README](https://github.com/lgse/strata#readme).

## What this fork adds

**Jump to name** ([`feat/jump-to-name`](https://github.com/iNViTiON/strata/tree/feat/jump-to-name)).
Type-ahead in the file list: typing letters jumps to the file whose name
starts with them. The "Default typing mode" preference chooses between this
and Vim keys (h, j, k, l as arrows, with `/` to start a jump).
This one is intended for upstream.

**Preload neighbor previews**
([`feat/preload-neighbor-previews`](https://github.com/iNViTiON/strata/tree/feat/preload-neighbor-previews)).
Opt-in (Settings → General → Performance). While the preview is open, the
entries above and below the selection are prepared in the background: images
and the first PDF page are rendered, and videos are decoded up to their first
frame and then held in a paused worker. In measurements, selecting a
preloaded neighbor took about 10 ms instead of 150–575 ms.

**Expanded preview**
([`feat/expanded-preview`](https://github.com/iNViTiON/strata/tree/feat/expanded-preview)).
Shift+Space swaps the Quick Preview into a large view without reloading it, as
an overlay or a fullscreen window. Images and PDFs zoom and pan, and video and
images switch to a sharper decode. With "Hold Shift to control the preview",
plain arrows keep moving through files and Shift+arrows drive the preview.

## Branches

| Branch | Contents |
| --- | --- |
| `main` | Untouched mirror of upstream |
| `nix-dev` | Default branch: `main` plus the Nix dev shell, the Nix package and the automation |
| `base/preview-seams` | Neutral preview hooks both preview features build on |
| `feat/*` | One feature each |
| `release` | `nix-dev` plus every feature, rebuilt automatically and tagged `fork-release-*` |

`main` and `nix-dev` follow upstream every six hours, and `release` is
rebuilt after them. Details are in [NIX.md](https://github.com/iNViTiON/strata/blob/nix-dev/NIX.md).

## Use it with Nix

Try it without installing:

```bash
nix run github:iNViTiON/strata/release \
  --extra-substituters https://invition.cachix.org \
  --extra-trusted-public-keys invition.cachix.org-1:UBnayz18duoQrchGIMu740K49/WVsaa8dThirDR/Hd4=
```

The two cache options download the prebuilt package instead of compiling it.
Nix only honors them for trusted users (`trusted-users` in `nix.conf`);
otherwise add the cache system-wide as below, or drop them and let it build.

Install it from a flake:

```nix
{
  inputs.strata.url = "github:iNViTiON/strata/release";

  # then, for example in an overlay:
  #   strata = inputs.strata.packages.${system}.strata;
  # (x86_64-linux and aarch64-linux are both built and cached)

  # binary cache for published releases
  nixConfig = {
    extra-substituters = [ "https://invition.cachix.org" ];
    extra-trusted-public-keys = [
      "invition.cachix.org-1:UBnayz18duoQrchGIMu740K49/WVsaa8dThirDR/Hd4="
    ];
  };
}
```

On NixOS, set the same cache in `nix.settings.substituters` and
`nix.settings.trusted-public-keys`. Leave out `inputs.nixpkgs.follows`: the
cache serves only the build pinned by this flake's `flake.lock`, and anything
else compiles from source. To pin a release, use a tag, for example
`github:iNViTiON/strata/fork-release-20260930-1`.

## Other distributions

Each release is also published on the
[Releases page](https://github.com/iNViTiON/strata/releases) as x86_64 and
aarch64 archives built the same way as upstream's, with checksums and build
attestations. The fork's `install.sh` installs the latest one:

```bash
curl -fsSL https://raw.githubusercontent.com/iNViTiON/strata/release/install.sh | bash
```

The in-app updater of these builds follows this fork's releases (the Nightly
channel). Coming from an upstream install, pick Nightly once under Settings →
Updates.

## Credits

- [Strata](https://github.com/lgse/strata) by lgse, MIT licensed. The bundled
  UnRAR code has its own license; see upstream's `THIRD_PARTY_LICENSES.md`.
- The Nix package is based on
  [Th1nkK1D's package](https://github.com/Th1nkK1D/nixos-config/blob/main/pkgs/strata/package.nix), MIT licensed.
