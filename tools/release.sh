#!/usr/bin/env bash
# Release helper behind `mise run changelog` and `mise run release`. See docs/releasing.md.
#   release.sh changelog
#   release.sh prepare <version> [--dry-run]
#   release.sh tag <version> [--dry-run]
set -euo pipefail

cd "$(dirname "$0")/.."

die() {
  echo "error: $*" >&2
  exit 1
}

cargo_version() {
  sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1
}

# Closest release tag before HEAD.
base_tag() {
  git describe --tags --abbrev=0 --match 'v[0-9]*' HEAD 2>/dev/null ||
    die "no release tag found. The first release's CHANGELOG.md section is written by hand."
}

# Notes for the commits after the base tag. Pass a version to render a release section.
notes() {
  local version=${1:-} base args=()
  base=$(base_tag)
  local bad
  bad=$(git log --format='%h %s' "$base..HEAD" | grep -Ev '^[0-9a-f]+ [a-z]+(\([^)]+\))?!?: ' || true)
  if [[ -n $bad ]]; then
    echo "warning: not Conventional Commits; they land under Maintenance:" >&2
    echo "$bad" >&2
  fi
  [[ -n $version ]] && args=(--tag "v$version")
  RUST_LOG=error git-cliff --config cliff.toml ${args[@]+"${args[@]}"} "$base..HEAD"
}

# X.Y.Z with no leading zeros and at most nine digits per part, so bash arithmetic can't overflow.
VERSION_RE='^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$'

# Succeeds when $1 > $2.
semver_gt() {
  [[ $1 =~ $VERSION_RE ]] || return 1
  local a=("${BASH_REMATCH[@]:1:3}") i
  [[ $2 =~ $VERSION_RE ]] || return 1
  local b=("${BASH_REMATCH[@]:1:3}")
  for i in 0 1 2; do
    ((a[i] > b[i])) && return 0
    ((a[i] < b[i])) && return 1
  done
  return 1
}

check_version() {
  [[ ${1:-} =~ $VERSION_RE ]] || die "'${1:-}' isn't a version like 0.2.0 (X.Y.Z, no leading zeros, no prerelease part)"
}

changelog_has() {
  grep -q "^## $1 " CHANGELOG.md
}

prepare() {
  local version=$1 dry=$2 current branch
  check_version "$version"
  [[ $(head -n 1 CHANGELOG.md) == "# Changelog" ]] || die "CHANGELOG.md should start with '# Changelog'"
  current=$(cargo_version)
  semver_gt "$version" "$current" || die "$version isn't greater than the current version $current"
  changelog_has "$version" && die "CHANGELOG.md already has a $version section; edit it by hand"
  branch=$(git branch --show-current)
  [[ $branch == wes/* ]] || die "run this on a wes/<task-name> branch, not '${branch:-detached HEAD}'"
  [[ -z $(git status --porcelain) ]] || die "the working tree isn't clean"

  local section
  section=$(notes "$version" | sed '/./,$!d')

  if [[ $dry == true ]]; then
    echo "Dry run: nothing written."
    echo
    echo "Cargo.toml: version \"$current\" -> \"$version\""
    echo "Cargo.lock: wayshare $current -> $version"
    echo "CHANGELOG.md: new section after '# Changelog':"
    echo
    echo "$section"
    return
  fi

  awk -v v="$version" '!done && /^version = "/ { $0 = "version = \"" v "\""; done = 1 } 1' Cargo.toml >Cargo.toml.new
  mv Cargo.toml.new Cargo.toml
  cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null
  {
    head -n 1 CHANGELOG.md
    echo
    echo "$section"
    tail -n +2 CHANGELOG.md
  } >CHANGELOG.md.new
  mv CHANGELOG.md.new CHANGELOG.md

  cat <<MSG
Prepared $version: Cargo.toml, Cargo.lock and CHANGELOG.md.

Next:
  1. Review the new CHANGELOG.md section for correctness and omissions, add upgrade
     notes, and run \$unslop on it (see AGENTS.md).
  2. Commit as 'chore(release): $version' and open the PR.
  3. After it merges, on main: mise run release $version --tag
MSG
}

tag() {
  local version=$1 dry=$2 sha ci
  check_version "$version"
  [[ $(git branch --show-current) == main ]] || die "tag releases from main"
  [[ -z $(git status --porcelain) ]] || die "the working tree isn't clean"
  git fetch --quiet origin main
  sha=$(git rev-parse HEAD)
  [[ $sha == "$(git rev-parse origin/main)" ]] || die "HEAD isn't origin/main; pull first"
  [[ $(cargo_version) == "$version" ]] || die "Cargo.toml is at $(cargo_version), not $version"
  changelog_has "$version" || die "CHANGELOG.md has no $version section"
  git rev-parse -q --verify "refs/tags/v$version" >/dev/null && die "tag v$version already exists locally"
  [[ -z $(git ls-remote --tags origin "refs/tags/v$version") ]] || die "tag v$version already exists on origin"
  ci=$(gh run list --workflow ci.yml --commit "$sha" --limit 1 --json conclusion --jq '.[0].conclusion // "none"')
  [[ $ci == success ]] || die "CI on $sha is '$ci', not success"

  if [[ $dry == true ]]; then
    echo "Dry run: checks passed; would tag v$version at $sha and push the tag."
    return
  fi
  local answer
  read -r -p "Tag v$version at $(git rev-parse --short "$sha") and push it to origin? [y/N] " answer </dev/tty
  [[ $answer == [yY] ]] || die "aborted"
  git tag -a "v$version" -m "wayshare $version" "$sha"
  git push origin "refs/tags/v$version"
  echo "Pushed v$version. The Release workflow now builds the draft."
}

case ${1:-} in
  changelog) notes ;;
  prepare) prepare "${2:-}" "${3:-false}" ;;
  tag) tag "${2:-}" "${3:-false}" ;;
  *) die "usage: release.sh changelog | prepare <version> [true|false] | tag <version> [true|false]" ;;
esac
