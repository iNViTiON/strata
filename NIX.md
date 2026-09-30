# Nix development environment (fork only)

This file, `flake.nix`, `flake.lock`, `.envrc`, `nix/`, `.release/`,
`.mise/tasks/release`, `.github/README.md` (the fork's landing page) and the
`sync-upstream.yml` and `assemble-release.yml` workflows exist only on the
fork's `nix-dev` branch (and `release`, built from it). `main` mirrors
`lgse/strata` exactly; never commit to it.

## Branches

| Branch | Purpose |
| --- | --- |
| `main` | Untouched mirror of upstream, fast-forwarded by the sync workflow to the latest stable release tag (or, on demand, upstream `main`'s tip) |
| `nix-dev` | `main` plus this environment; stays checked out in the main checkout |
| `release` | Generated: `nix-dev` plus the branches in `.release/branches`; never commit to it by hand |
| feature branches | Created from `main` in worktrees under `.claude/worktrees/`, using this checkout's environment |

## Using the shell

```bash
direnv allow                # once; afterwards the shell loads on cd
nix develop                 # or enter it explicitly
mise run check              # fmt, check, clippy, test, deny, typos
./scripts/check.sh
./scripts/test-headless.py
./scripts/e2e.sh            # runs in the pinned image via the shell's rootless podman
mise run dev                # or: mise run start-dev
```

The shell provides everything `mise install` would, plus the native libraries:

- **Rust** from rust-overlay at the exact version and components in `mise.toml`
  (`tools.rust`), plus `rust-src`. After upstream bumps Rust, run
  `nix flake update rust-overlay` if the new version is not yet known.
- **cargo-deny, typos, cargo-watch** from the exact URLs and checksums in
  upstream's `mise.lock`, so they follow upstream bumps with no edits here.
- **mise** as the upstream static binary, because nixpkgs' version is older
  than `min_version` in `mise.toml`. Bump `version`/`hash` in `flake.nix` when
  upstream raises `min_version`.
- GTK 4, GtkSourceView 5, Poppler, GStreamer (base, good), Xvfb, AT-SPI,
  D-Bus, ImageMagick, bubblewrap, ffmpeg, ffmpegthumbnailer, util-linux,
  podman and Python 3.14 with PyGObject and pycairo.

### How mise is kept off the network

mise still reads `mise.toml` for tasks, but the shell sets:

- `MISE_DISABLE_TOOLS` to every tool listed under `[tools]` in `mise.toml`
  (the `disable_tools` setting), so mise neither installs nor shims them and
  tasks use the Nix binaries on `PATH`. Prebuilt mise downloads would not run on
  NixOS anyway.
- `MISE_*_AUTO_INSTALL=false` as a second guard.
- `MISE_TRUSTED_CONFIG_PATHS` to the main checkout, which also trusts every
  worktree below it.

No mise setting is written to `mise.toml`, so upstream files stay untouched.

### The `cargo` wrapper

`scripts/test-headless.py` passes only `PATH` to its `cargo` child and forces
`XDG_DATA_DIRS=/usr/local/share:/usr/share`, which is empty on NixOS. The
shell's `cargo` is therefore a wrapper that:

- always sources the stdenv build environment (`PKG_CONFIG_PATH`,
  `NIX_CFLAGS_COMPILE`, `NIX_LDFLAGS`), so build scripts get identical inputs
  from every caller and nothing rebuilds when switching between scripts;
- appends the Nix GTK, GSettings, icon and MIME data to `XDG_DATA_DIRS`;
- sets `GI_TYPELIB_PATH`, `GST_PLUGIN_SYSTEM_PATH_1_0`,
  `GDK_PIXBUF_MODULE_FILE`, `FONTCONFIG_FILE` and `TZDIR` when unset.

The AT-SPI daemons are also added to `PATH`, because the headless harness looks
for them in FHS `libexec` paths first and falls back to `PATH`.

### Podman for `scripts/e2e.sh`

`e2e.sh` prefers podman, and only its `--userns=keep-id` path keeps the
bind-mounted checkout writable; rootless Docker maps the container user to a
subuid and fails with `Permission denied` on `/workspace/target`. The shell's
`podman` needs the system's setuid `newuidmap`/`newgidmap` and a subuid range
(both present), but not `virtualisation.podman`: NixOS then has no
`/etc/containers/policy.json`, and podman reads a user policy only from
`$HOME/.config/containers`. The wrapper therefore runs podman with
`HOME=~/.cache/strata-podman-home` (holding an accept-anything policy, the
NixOS default) and keeps image storage in `~/.local/share/containers`.
`scripts/e2e_base.py` pulls the tag pinned by the E2E inputs from
`ghcr.io/lgse` and checks its input-key label; signatures are not checked.

### Hard-coded FHS paths

`sandbox::tests::staged_secrets_are_inherited_only_when_explicitly_mapped_to_stdin`
spawns `/bin/cat`, which NixOS does not provide. A per-repo `/bin` cannot be
faked without root: a private mount namespace needs an unprivileged user
namespace, which shows root-owned paths (`/`, `/nix/store`) as `nobody`, and
Strata's executable-ownership checks then fail 29 `portal_setup` tests. The fix
is one system-level symlink in the NixOS configuration (preferred over
`services.envfs`, which mounts FUSE over all of `/bin` and `/usr/bin`):

```nix
systemd.tmpfiles.rules = [ "L+ /bin/cat - - - - /run/current-system/sw/bin/cat" ];
```

Drop it once upstream's test resolves `cat` through `PATH`.

Real-app previews on NixOS additionally need `bubblewrap` in
`environment.systemPackages` (Strata resolves `bwrap` only from system paths),
and the preview sandbox binds `/usr` and `/lib` but not `/nix/store`, so
sandboxed helpers likely still fail there until upstream supports it.

## Worktrees

Worktrees live in `.claude/worktrees/`, which is listed in `.git/info/exclude`
(not `.gitignore`). They check out `main`, which has no flake, so they borrow
this checkout's environment:

```bash
git worktree add .claude/worktrees/my-feature -b feat/123-my-feature main
cd .claude/worktrees/my-feature

# Either: direnv finds the parent .envrc
direnv exec . mise run check

# Or: point Nix at the main checkout explicitly
nix develop /home/hisoft/Documents/strata -c mise run check
```

Shells that do not run direnv hooks (for example Claude sessions) must use one
of the explicit forms above. Commands run in the worktree's directory, so
`cargo` builds the worktree's sources into its own `target/`.

Remove a worktree with `git worktree remove .claude/worktrees/my-feature`.

## Keeping `nix-dev` in sync

`.github/workflows/sync-upstream.yml` runs every six hours and on demand. It
fast-forwards `main` to upstream's latest stable release tag (`vX.Y.Z`; release
candidates and nightlies are ignored), rebases `nix-dev` onto it and pushes both
atomically with `--force-with-lease`. `main` never moves backwards: when it is
already at or past that tag (for example after a tip sync), the run does
nothing. On a conflict it pushes nothing and opens (or updates) an issue
labeled `sync-conflict` with resolution steps.

To follow upstream `main`'s tip instead, run the workflow manually with
`main_tip` enabled:

```bash
gh workflow run sync-upstream.yml -R iNViTiON/strata -f main_tip=true
```

Because the workflow rewrites `nix-dev`, update the local checkout after a sync:

```bash
git fetch origin
git reset --keep origin/nix-dev    # while nix-dev is checked out
git branch -f main origin/main
```

Keep fork-only changes in new files where possible; the only upstream file
touched is one `.gitignore` line (`/.direnv/`), which keeps rebases trivial.

## Releases

`release` is what the NixOS configuration installs. It is rebuilt from scratch,
never edited: `nix-dev`, then each branch in `.release/branches` merged in
order with `--no-ff`, then a commit recording `.release/manifest` (the SHAs of
`nix-dev` and every merged branch). Every published build is tagged
`fork-release-YYYYMMDD-N`, so a revision pinned in a `flake.lock` stays
fetchable after `release` is force-pushed.

### The package

`nix build .#strata` (also `.#default`) builds this tree with the dev shell's
Rust toolchain and `Cargo.lock`, so there is no `cargoHash` to maintain. It is
based on Th1nkK1D's package (credit and MIT notice in `nix/package.nix`). The
patches point the preview sandbox at the store instead of `/usr` in every
`src/sandbox*` call site, pin `bwrap` and `prlimit`, give the helpers a store
`PATH`, and bind `/run/opengl-driver` so VA-API works in the sandbox. Each
uses `--replace-fail`, so an upstream change to those lines fails the build
instead of silently dropping a patch. UnRAR is statically linked and unfree;
the flake allows unfree for `strata` only.

To install it from the NixOS configuration:

```nix
inputs.strata.url = "github:iNViTiON/strata/release";

# in an overlay
strata = inputs.strata.packages.${prev.stdenv.hostPlatform.system}.strata;

# the fork's binary cache
nix.settings = {
  substituters = [ "https://invition.cachix.org" ];
  trusted-public-keys = [ "invition.cachix.org-1:UBnayz18duoQrchGIMu740K49/WVsaa8dThirDR/Hd4=" ];
};
```

`nix flake update strata` then picks up the latest release. The VA-API bind is
already in the package; do not add it again in an overlay.

The flake provides the package and the dev shell for `x86_64-linux` and
`aarch64-linux`. CI uploads every release it publishes to the `invition` Cachix
cache for both (only the paths cache.nixos.org does not already serve): the
`assemble` job for x86_64 after its checks, and the `nix-aarch64` job on an ARM
runner, which builds the same published commit without rerunning the checks. A download needs the exact
derivation CI built, so do not set `inputs.nixpkgs.follows` (or override
`rust-overlay`): the fork's own `flake.lock` must drive the build. With
`follows`, the package still builds, just from source. Releases published
locally with `--publish` are not uploaded; the next CI release is.

### Adding or removing a feature

Edit `.release/branches` on `nix-dev` (order matters: a branch that builds on
another comes after it), commit, and push `nix-dev`. Feature branches must be
pushed to `origin` before they are listed: CI merges `origin/*` and fails
rather than building a release without a listed branch. Branches built on
`base/preview-seams` are rebased with it; keep that base first in the list.

### Rebuilding locally

Use a dedicated worktree; the script refuses to run in the `nix-dev` or `main`
checkout because it switches branches:

```bash
git worktree add --detach .claude/worktrees/release
cd .claude/worktrees/release
direnv exec . mise run release                        # local branches, no checks
direnv exec . mise run release -- --checks local      # + fmt, clippy, deny, typos, tests, nix build, e2e
direnv exec . mise run release -- --source origin --checks local --publish
```

`--source local` (the default) merges local branches, which works before they
are pushed. `--publish` requires `--source origin`, tags the result and pushes
`release` and the tag with `--force-with-lease`. Set `RELEASE_MEMORY_MAX=16G`
to run the heavy local checks inside a `systemd-run` memory cap.
`.release/assemble.sh --help` lists every option and exit code.

### Conflicts

Before merging, the script enables `rerere` and trains it
(`contrib/rerere-train.sh`) from the merges of the published `release` and the
latest tags, so every conflict resolved in an earlier release is replayed. A
merge is committed automatically only when `rerere` resolved every hunk. When
it stops:

1. Resolve the listed files. If the right result depends on what a feature is
   meant to do, ask that branch's owner.
2. `git add` them and `git commit --no-edit`; `rerere` records the resolution.
3. Run the script again. It starts from `nix-dev`, discards the half-built
   branch and replays the recorded resolution.
4. Publish that rebuild. CI has no `rr-cache` of its own and learns the
   resolution only from published release merges.

A conflict that keeps returning belongs in the feature branch: rebase it and
drop the resolution.

### CI

`.github/workflows/assemble-release.yml` ("Assemble fork release") runs after
a successful "Sync upstream", every six hours, and on demand. It skips when
the published manifest already matches, otherwise assembles from `origin/*`,
runs upstream's pinned-container `scripts/quality.sh` plus `deny`, `typos`,
the script tests and `nix build .#strata`, then tags and pushes with
`SYNC_PAT` (`GITHUB_TOKEN` cannot push commits that touch workflows) and
uploads the package to Cachix (`CACHIX_AUTH_TOKEN`, `CACHIX_SIGNING_KEY`). On a
conflict or failure it pushes nothing and opens or updates an issue labeled
`release-failed`. A `push` trigger on feature branches cannot work: GitHub
reads it from the pushed branch's copy of the workflow, and feature branches
come from `main`; the schedule picks their pushes up instead.

Upstream's publishing workflows cannot fire from `release` or its tags:
`release.yml` is `workflow_dispatch` only, and `packaging.yml`,
`publish-aur.yml`, `e2e-images.yml` and `ci.yml` trigger only on pushes to
`main` or pull requests. None has a tag, `create` or `release` trigger.

On top of that, every upstream workflow is disabled in the fork's Actions
settings (`gh workflow disable`); only "Sync upstream" and "Assemble fork
release" run. The setting is per workflow file, so a workflow upstream adds
later starts out enabled: disable it with
`gh workflow disable <file> -R iNViTiON/strata`.

### Binaries for other distributions

Every published release also gets a version tag in upstream's nightly form,
`v<next patch>-nightly.YYYYMMDD[.N]` (computed by `scripts/release_version.py`),
because the in-app updater and `install.sh` only understand that grammar. The
workflow's `binaries` jobs build it natively on Ubuntu for x86_64 and aarch64,
exactly like upstream's `release.yml` (same archive names and layout, with
`STRATA_RELEASE_TAG`, `STRATA_BUILD_KIND=nightly` and the commit baked in),
attest the archives, and publish a GitHub Release marked latest. They pick the
newest version tag that has no release yet, so a failed build is retried by
the next run even when nothing else changed.

`fork/distribution` (from `main`, last in `.release/branches`, never
upstreamed) points the updater, its Settings links and `install.sh` at this
fork, has `install.sh` accept the nightly tags, and makes Nightly the default
update channel, since the fork publishes nothing else. Users coming from an
upstream install keep their saved channel and must choose Nightly once in
Settings. The Nix package ignores all of this: its install-source marker
disables the updater.

### Rolling back

Point the NixOS input at a tag instead of the branch:

```nix
inputs.strata.url = "github:iNViTiON/strata/fork-release-20261001-1";
```

or rebuild with the old revision locked:
`nix flake lock --override-input strata github:iNViTiON/strata/<tag>`. To move
`release` itself back, push the tag over it:

```bash
git push --force-with-lease origin fork-release-20261001-1^{commit}:refs/heads/release
```

The next CI run rebuilds it from the current branches unless the manifest
matches, so fix or delist the offending branch first.
