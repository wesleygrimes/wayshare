# Releasing

Releases are cut by the maintainer. Notes are generated from Conventional Commits with
[git-cliff](https://git-cliff.org) (`cliff.toml`), reviewed by hand, then published from a
draft.

## The first-release boundary

v0.1.0 is the changelog boundary. Its `CHANGELOG.md` section is handwritten and stays as it
is. The commits before it aren't Conventional Commits and are never parsed. The v0.1.0
draft stays unpublished because its tag predates binary stripping; v0.1.1 is the first
public release.

Generation reads only `<latest release tag>..HEAD`. With no `v*` tag reachable, the
tooling stops and tells you to write the section by hand, so it can't fall back to parsing
the old history. Commits in the range that aren't Conventional Commits are listed as a
warning and land under Maintenance.

## Sections

Breaking changes, Features, Fixes, Performance, Documentation, Maintenance. Empty ones are
omitted. Maintenance holds `ci`, `build`, `chore`, `test`, `style`, `refactor`, `revert` and
dependency bumps. A `(security)` scope always goes under Fixes, and breaking changes always
lead, whatever their type. A `BREAKING CHANGE:` footer prints under its entry as the
upgrade instruction.

## Cutting a release

1. Branch as `wes/release-X.Y.Z`, then run `mise run release X.Y.Z --dry-run`. It runs
   every check and prints the changes without writing.
2. Run `mise run release X.Y.Z`. It bumps `Cargo.toml` and `Cargo.lock` and prepends the
   generated section to `CHANGELOG.md`. It refuses if the section already exists, so
   reviewed wording is never overwritten. The date is the UTC date of preparation; edit it if
   needed.
3. Review the section for correctness and omissions, add upgrade notes, and run `$unslop`
   on it ([AGENTS.md](../AGENTS.md) has the writing rule). Commit as
   `chore(release): X.Y.Z` and open the PR.
4. After the PR merges and CI passes on `main`, run `mise run release X.Y.Z --tag` from
   an up-to-date `main`. It checks the version, changelog section, existing tags and CI
   status, asks for confirmation, then creates the annotated `vX.Y.Z` tag and pushes only
   that tag.
5. The Release workflow builds both architectures and creates a draft release with the
   changelog section as its notes.
6. Check the draft: both archives, `sha256sums.txt`, the rendered notes, and (public
   repo) `gh attestation verify <archive> -R wesleygrimes/wayshare`. Then publish it by hand.

`mise run changelog` previews the unreleased notes and writes nothing.

## Attestations

When the repository is public, the build attests each archive, a failed attestation fails
the run, and the release job verifies every archive before touching the draft. The notes
mention attestations only in that case. On a private repository the build skips them.

## Recovering a failed run

Run the Release workflow on the tag itself: `gh workflow run release.yml --ref vX.Y.Z`,
using a tag that contains this workflow, such as v0.1.1. The run builds that tag's commit,
and the attestations record the tag's SHA and ref. It fails unless the ref is a `vX.Y.Z`
tag. It then creates the draft or updates the existing draft's assets (`--clobber`) and
notes. It refuses a published release and never creates, moves or deletes a tag. Workflow
reruns reuse the original event, so they can't pick up changes.

## Published tags

Never rewrite, move or delete a published tag. If a release is wrong, ship a new version.
