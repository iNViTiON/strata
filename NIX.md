# Nix development environment (fork only)

This file, `flake.nix`, `flake.lock`, `.envrc` and
`.github/workflows/sync-upstream.yml` exist only on the fork's `nix-dev` branch.
`main` mirrors `lgse/strata` exactly; never commit to it.

## Branches

| Branch | Purpose |
| --- | --- |
| `main` | Untouched mirror of upstream `main`, fast-forwarded by the sync workflow |
| `nix-dev` | `main` plus this environment; stays checked out in the main checkout |
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
is system-level, for example `services.envfs.enable = true;` or a tmpfiles
symlink `L+ /bin/cat - - - - /run/current-system/sw/bin/cat`.

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
fast-forwards `main` to upstream, rebases `nix-dev` onto it and pushes both
atomically with `--force-with-lease`. On a conflict it pushes nothing and opens
(or updates) an issue labeled `sync-conflict` with resolution steps.

Because the workflow rewrites `nix-dev`, update the local checkout after a sync:

```bash
git fetch origin
git reset --keep origin/nix-dev    # while nix-dev is checked out
git branch -f main origin/main
```

Keep fork-only changes in new files where possible; the only upstream file
touched is one `.gitignore` line (`/.direnv/`), which keeps rebases trivial.
