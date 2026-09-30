#!/usr/bin/env bash
# Rebuilds the fork's release branch: nix-dev plus every branch listed in
# .release/branches, merged in order. See "Releases" in NIX.md.
set -euo pipefail

# Checking out other commits rewrites this file while bash is still reading it.
if [[ -z "${RELEASE_ASSEMBLE_COPY:-}" ]]; then
  copy="$(mktemp -t fork-release-assemble.XXXXXX)"
  cp "$0" "$copy"
  RELEASE_ASSEMBLE_COPY="$copy" exec bash "$copy" "$@"
fi
trap 'rm -f "$RELEASE_ASSEMBLE_COPY"' EXIT

usage() {
  cat <<'EOF'
Usage: .release/assemble.sh [options]

  --source local|origin  Merge local branches (default) or the remote's copies.
  --if-changed           Stop when the published manifest already matches.
  --checks none|local|ci Run checks on the assembled tree (default: none).
  --publish              Tag and push the release (requires --source origin).
  --no-fetch             Skip fetching the remote.
  -h, --help             Show this help.

Environment (mostly for testing): RELEASE_REMOTE (origin), RELEASE_BASE
(nix-dev), RELEASE_BRANCH (release), RELEASE_BRANCHES_FILE (read the list from
this file instead of the base commit), RELEASE_TAG_PREFIX (fork-release-),
RELEASE_MEMORY_MAX (cap heavy local checks with systemd-run, e.g. 16G).

Exit codes: 0 done or up to date, 2 unresolved conflict, 3 checks failed,
4 publish refused, 1 other errors.
EOF
}

remote="${RELEASE_REMOTE:-origin}"
base_name="${RELEASE_BASE:-nix-dev}"
release="${RELEASE_BRANCH:-release}"
tag_prefix="${RELEASE_TAG_PREFIX:-fork-release-}"
source=local
if_changed=false
checks=none
publish=false
fetch=true

while (($#)); do
  case "$1" in
    --source) source="${2:?}"; shift ;;
    --if-changed) if_changed=true ;;
    --checks) checks="${2:?}"; shift ;;
    --publish) publish=true ;;
    --no-fetch) fetch=false ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; exit 1 ;;
  esac
  shift
done
case "$source" in local | origin) ;; *) echo "error: --source must be local or origin" >&2; exit 1 ;; esac
case "$checks" in none | local | ci) ;; *) echo "error: --checks must be none, local or ci" >&2; exit 1 ;; esac
if $publish && [[ "$source" != origin ]]; then
  echo "error: --publish needs --source origin, so every published commit is already on $remote" >&2
  exit 4
fi

cd "$(git rev-parse --show-toplevel)"
say() { printf '\n==> %s\n' "$*"; }
output() { if [[ -n "${GITHUB_OUTPUT:-}" ]]; then echo "$1" >> "$GITHUB_OUTPUT"; fi; }

if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
  echo "error: the worktree has uncommitted changes; commit or set them aside first" >&2
  exit 1
fi
case "$(git branch --show-current)" in
  "$base_name" | main)
    echo "error: run this from a dedicated worktree, not the $(git branch --show-current) checkout:" >&2
    echo "  git worktree add --detach .claude/worktrees/release && cd .claude/worktrees/release" >&2
    exit 1 ;;
esac

if $fetch; then
  say "Fetching $remote"
  git fetch --prune --no-tags "$remote" "+refs/heads/*:refs/remotes/$remote/*"
  git fetch --no-tags "$remote" "+refs/tags/$tag_prefix*:refs/tags/$tag_prefix*"
fi

prefix=""
[[ "$source" == origin ]] && prefix="$remote/"
ref_of() { git rev-parse --verify --quiet "refs/remotes/$1^{commit}" 2> /dev/null || git rev-parse --verify --quiet "$1^{commit}"; }
resolve() {
  local name="$1" ref="${prefix}$1" sha
  if ! sha="$(ref_of "$ref")"; then
    if [[ "$source" == origin ]]; then
      echo "error: $name is not on $remote; push it or remove it from .release/branches" >&2
    else
      echo "error: no local branch $name" >&2
    fi
    exit 1
  fi
  echo "$sha"
}

base_sha="$(resolve "$base_name")"
if [[ -n "${RELEASE_BRANCHES_FILE:-}" ]]; then
  list="$(cat "$RELEASE_BRANCHES_FILE")"
else
  list="$(git show "$base_sha:.release/branches")"
fi
mapfile -t branches < <(sed -e 's/#.*//' -e 's/[[:space:]]*$//' -e '/^$/d' <<< "$list")

manifest="$base_name $base_sha"
declare -A shas
for branch in "${branches[@]}"; do
  shas[$branch]="$(resolve "$branch")"
  manifest+=$'\n'"$branch ${shas[$branch]}"
done
say "Manifest"
echo "$manifest"

published="$(git rev-parse --verify --quiet "refs/remotes/$remote/$release" || true)"
published_manifest=""
if [[ -n "$published" ]]; then
  published_manifest="$(git show "$published:.release/manifest" 2> /dev/null | sed '/^#/d' || true)"
fi
if $if_changed && [[ "$manifest" == "$published_manifest" ]]; then
  say "$remote/$release already matches this manifest; nothing to do"
  output "changed=false"
  exit 0
fi
output "changed=true"

find_rerere_train() {
  local candidate
  for candidate in \
    "${GIT_RERERE_TRAIN:-}" \
    "$(git --exec-path)/../../share/git/contrib/rerere-train.sh" \
    /usr/share/doc/git/contrib/rerere-train.sh \
    /usr/share/git/contrib/rerere-train.sh \
    /usr/share/git-core/contrib/rerere-train.sh; do
    if [[ -n "$candidate" && -f "$candidate" ]]; then
      echo "$candidate"
      return
    fi
  done
  return 1
}

# Keep rerere to this worktree: other worktrees may be mid-rebase.
scope=()
if [[ "$(git config --bool extensions.worktreeConfig || true)" == true ]]; then
  scope=(--worktree)
elif [[ "$(git rev-parse --git-common-dir)" != "$(git rev-parse --git-dir)" ]]; then
  echo "warning: enabling rerere repository-wide; set extensions.worktreeConfig to limit it" >&2
fi
git config "${scope[@]}" rerere.enabled true
git config "${scope[@]}" rerere.autoUpdate true
training=()
[[ -n "$published" ]] && training+=("$published")
git rev-parse --verify --quiet "refs/heads/$release" > /dev/null && training+=("refs/heads/$release")
mapfile -t -O "${#training[@]}" training < <(git tag --list "$tag_prefix*" --sort=-creatordate | head -n 5)
if ((${#training[@]})); then
  if trainer="$(find_rerere_train)"; then
    say "Training rerere from earlier releases"
    # Only fork merges matter; upstream history has none to replay.
    main_sha="$(ref_of "${prefix}main" || true)"
    sh "$trainer" "${training[@]}" ${main_sha:+"^$main_sha"} > /dev/null
  else
    echo "warning: rerere-train.sh not found (set GIT_RERERE_TRAIN); relying on the local rr-cache" >&2
  fi
fi

say "Rebuilding $release from $base_name ($base_sha)"
git switch --quiet -C "$release" "$base_sha"

for branch in "${branches[@]}"; do
  say "Merging $branch (${shas[$branch]})"
  message="Merge branch '$branch' into $release"
  if git merge --no-ff --no-edit -m "$message" "${shas[$branch]}"; then
    continue
  fi
  unmerged="$(git diff --name-only --diff-filter=U)"
  remaining="$(git rerere remaining)"
  if [[ -z "$unmerged" && -z "$remaining" ]] \
    && ! git diff --cached | grep -qE '^\+(<{7}|>{7})( |$)'; then
    echo "rerere resolved every conflict; committing"
    git commit --no-edit --quiet
    continue
  fi
  output "conflict_branch=$branch"
  {
    echo "conflict_files<<EOF"
    echo "${unmerged:-$remaining}"
    echo "EOF"
  } >> "${GITHUB_OUTPUT:-/dev/null}"
  cat >&2 <<EOF

Merging $branch into $release conflicts in:
$(sed 's/^/  /' <<< "${unmerged:-$remaining}")

Resolve it once; rerere records the result and replays it on every rebuild:
  1. Fix the files above (ask the branch owner if the intent is unclear).
  2. git add <files> && git commit --no-edit
  3. Run this script again. It starts from $base_name, so the half-built
     $release is discarded, but the recorded resolution is reused.
     Publish the rebuilt release so CI can train on the resolution too.
To give up instead: git merge --abort
EOF
  exit 2
done

say "Recording the manifest"
{
  echo "# Fork release manifest: $base_name and the merged branches, in order."
  echo "$manifest"
} > .release/manifest
git add .release/manifest
git commit --quiet -m "chore(release): record manifest" -m "$manifest"
output "sha=$(git rev-parse HEAD)"

capped() {
  if [[ -n "${RELEASE_MEMORY_MAX:-}" ]]; then
    systemd-run --user --scope --quiet -p "MemoryMax=$RELEASE_MEMORY_MAX" -- "$@"
  else
    "$@"
  fi
}
nix_build() { nix build .#strata --no-link --max-jobs 1 --cores "${RELEASE_NIX_CORES:-4}" -L; }

run_checks() {
  local task
  case "$checks" in
    local)
      for task in fmt compile clippy deny typos scripts; do capped mise run "$task"; done
      capped ./scripts/test-headless.py
      nix_build
      capped ./scripts/e2e.sh
      ;;
    ci)
      ./scripts/quality.sh all
      for task in deny typos scripts; do mise run "$task"; done
      nix_build
      ;;
  esac
}
if [[ "$checks" != none ]]; then
  say "Running $checks checks"
  if ! run_checks; then
    echo "error: checks failed; $release was not published" >&2
    exit 3
  fi
fi

if $publish; then
  day="$(date -u +%Y%m%d)"
  n=1
  while git rev-parse --verify --quiet "refs/tags/$tag_prefix$day-$n" > /dev/null; do n=$((n + 1)); done
  tag="$tag_prefix$day-$n"
  say "Publishing $release as $tag"
  git tag -a "$tag" -m "Fork release $tag" -m "$manifest"
  if ! git push --atomic "--force-with-lease=refs/heads/$release:$published" \
    "$remote" "refs/heads/$release:refs/heads/$release" "refs/tags/$tag"; then
    git tag -d "$tag" > /dev/null
    echo "error: push refused; $remote/$release moved since the fetch" >&2
    exit 4
  fi
  output "tag=$tag"
fi

say "Done: $release is $(git rev-parse --short HEAD)"
