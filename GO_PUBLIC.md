# Going public

Make wayshare easy to use, review, release, and hand over to another maintainer.
Keep the code and process small enough that a person can understand both.

This is an implementation plan. Check items off after verifying the result.
GitHub settings, publication, and release actions still need explicit authorization
when this plan is carried out.

## Current baseline

Inspected on 2026-10-15:

- One Rust package with a library and binary, organized into existing modules.
  Roughly 22,000 lines under `src`, including about 4,600 generated libva bindings.
- `Cargo.lock`, an Apache-2.0 license, a pinned Rust toolchain, installation docs,
  and an initial `CHANGELOG.md` are already present.
- `mise.toml` exposes `build`, `test`, and `gen-vaapi-sys`. `build` currently uses
  the release profile.
- The release workflow tests and packages Linux x86_64 and ARM64 on `v*` tags,
  creates checksums, supports public build attestations, and creates a draft release.
  Its notes come from the handwritten changelog. Asset names omit the version,
  matching the installer's latest-release URLs. There is no PR CI workflow.
- The root `install.sh` downloads and checks release archives and runs
  `wayshare install-service`. Source installs use `cargo install --locked --path .`
  followed by `wayshare install-service`; distro packaging uses plain install
  commands. The old `tools/install.sh` and install/uninstall mise tasks are gone.
- `docs/development.md` explicitly says not to run crate-wide `cargo fmt`.
- No repository `AGENTS.md`, contribution guide, issue templates, PR template,
  or funding configuration is present.
- Live GitHub settings have not been audited. Treat their desired state below
  as work to verify, rather than an assertion about the current settings.

### Reported release validation

Wes shared the following completed checks from the release work. These are
reported results, not tests rerun while writing this plan:

| Area | Reported result | Remaining check |
| --- | --- | --- |
| Draft `v0.1.0` | Created with both architecture archives and `sha256sums.txt`; tests ran on both native CI runners. | Read back the live draft, workflow results, and asset list before launch. |
| Linux x86_64 | CI binary installed and ran on the Arch VM. | Repeat against the final published assets. |
| Linux ARM64 | Binary identified as a valid aarch64 executable. | Runtime installation and use on ARM hardware remain unverified. |
| Curl installer | Fresh install passed using a local copy of the assets; a tampered checksum blocked installation. | Test the actual anonymous GitHub download path after publication. |
| Service path | Plain install uses its binary path; a mise-style layout uses the shim. | Test service startup and upgrade behavior after a real mise install. |
| Shell/workflow lint | ShellCheck and actionlint passed. | Expose and enforce these checks through mise and PR CI. |
| Mise installation | No real `mise use -g github:wesleygrimes/wayshare` test yet. | Test against a public, published release. |

The repo was private and the release was a draft when these results were shared.
The installer URL and latest-release download returned 404. Making the repository
public alone will not make a draft available through the installer's latest URL.

**Launch blocker:** the current draft has no attestations because its private-repo
build skipped that step, but its Verify section says attestations exist. Rebuild
and verify them before making that claim in the published release.

## 1. Set the working agreements

- [ ] Add a short root `AGENTS.md`, aiming for about one page. Include the rules
  that affect everyday decisions; link to detailed developer and release docs.
  Cover:
  - Work inline unless Wes explicitly asks for delegation.
  - Keep changes within the request. Describe nontrivial edits before making them.
  - Prefer simple functions, clear ownership, and existing patterns. Add an
    abstraction or dependency only when it solves a concrete problem.
  - Comment on intent, protocol constraints, and safety requirements. Avoid
    comments that narrate obvious code. Explain the invariants around unsafe FFI.
  - Preserve generated files through their generator.
  - Use `wes/<task-name>` branches, Conventional Commits, and the mise tasks.
  - Run the checks appropriate to a change and report what actually ran, including
    checks that could not run on the available machine.
  - Get authorization before external writes unless the action was explicitly requested.
- [ ] Put the writing standard in `AGENTS.md`, the contribution guide, and the
  release instructions through one shared rule with links, rather than three
  long copies: **all project writing is for humans; no AI slop.**
  - Lead with the concrete behavior, problem, or benefit.
  - Use plain words and short, specific sentences. Explain why when it helps.
  - PR descriptions state what changed, why, and how it was verified. Scale the
    detail to the change.
  - Release notes explain what users will notice and any upgrade action.
  - Remove hype, filler, canned summaries, tool branding, and inflated claims.
    Never invent test results, performance gains, or compatibility claims.
  - Review generated text as carefully as code. A passing linter cannot establish
    whether writing is useful.
- [ ] Make `$unslop` a required review step for every new or changed document or
  template before handoff or publication. Cover the README, `AGENTS.md`, contributor
  and security guides, `docs/`, issue and PR templates, release-note templates,
  and generated release notes. Add this requirement to the short `AGENTS.md` and
  link to it from the contribution and release instructions.
  Read the installed skill's `SKILL.md`, scan for its writing patterns, revise,
  and self-audit. Preserve technical meaning, commands, links, and template syntax;
  verify those still work after editing. Report which files received the pass.
- [ ] Run an initial `$unslop` pass over all existing docs and templates, including
  this plan, before launch. Keep future passes focused on changed content. If the
  skill is unavailable, report the missing review rather than marking it complete.
- [ ] Add a concise `CONTRIBUTING.md` covering issue reports, who can open PRs,
  branch names, commit format, setup, checks, and review expectations. Keep the
  actual build instructions in `docs/development.md`.
- [ ] Add a short PR template prompting for the problem/result and verification.
  Include an issue link when relevant; avoid a checklist that overwhelms small fixes.

**Done when:** a new maintainer can find the rules and the relevant commands without
reading a large policy document.

## 2. Enforce Conventional Commits

- [ ] Require commit messages and PR titles in the form
  `<type>(optional-scope): <description>`, following the
  [Conventional Commits specification](https://www.conventionalcommits.org/en/v1.0.0/).
  Use lowercase types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`,
  `ci`, `chore`, `style`, and `revert`. Use scopes only when they clarify the change.
- [ ] Require clear descriptions, for example:

  ```text
  fix(clipboard): preserve copied images when reconnecting
  perf(encode): reuse buffers between video frames
  docs: explain which firewall ports to open
  ```

- [ ] Recognize both `!` and `BREAKING CHANGE:`. A breaking change must describe
  who is affected and what they need to do. Document the versioning policy while
  wayshare is below 1.0; the maintainer chooses the release version explicitly.
- [ ] Add `mise check:commits` to validate new commits against a specified base.
  Validate PR titles in CI as well, including after a title edit. Prefer existing
  pinned tooling over maintaining an incomplete parser or adding a separate
  runtime solely for commit linting.
- [ ] Use squash merges and configure the squash message to use the PR title.
  Review the final message before merging so release notes receive a useful entry.
  Preserve breaking-change details in the final commit body.
- [ ] Apply enforcement from an explicit adoption boundary. Existing commits use
  another style; preserve that history and the initial release notes. Release
  generation must not silently drop older changes or fail on the entire old history.

**Done when:** an invalid new commit or PR title fails a check, and a valid squash
commit produces the expected release-note category.

## 3. Make mise the development interface

- [ ] Pin the required tools and Rust components. Add `mise setup` to install
  managed tools and report missing system dependencies, with no implicit sudo.
  A fresh checkout must not depend on `mise.local.toml` or Wes's machine paths.
- [ ] Establish this small set of commands:

  | Command | Contract |
  | --- | --- |
  | `mise setup` | Prepare managed tools and explain OS prerequisites. |
  | `mise fmt` | Apply the agreed formatting to maintained source. |
  | `mise fmt:check` | Check formatting without changing files. |
  | `mise lint` | Run Rust, shell, and workflow linters. |
  | `mise test` | Run the locked Rust test suite. |
  | `mise check` | Run formatting checks, lint, and tests without rewriting source. |
  | `mise check:commits` | Validate commits introduced since the supplied base. |
  | `mise build` | Build with the development profile for quick iteration. |
  | `mise build --release` | Build the optimized distributable with locked dependencies. |
  | `mise audit` | Check dependency advisories, licenses, and sources. |
  | `mise changelog` | Preview generated notes for changes since the release baseline. |
  | `mise release <version> --dry-run` | Validate and preview release preparation. |
  | `mise release <version>` | Prepare the local version and generated changelog changes. |
  | `mise release <version> --tag` | Validate the merged release commit and push its tag when authorized. |

  Keep `gen-vaapi-sys` as the specialized binding-generation task. These are
  proposed interfaces, not commands that all exist today.
- [ ] Make the change from today's release-only `build` explicit in the developer
  docs. Put implementation helpers in small scripts only when task bodies become
  difficult to read. Let mise compose the workflow.
- [ ] Use `mise run <task>` in scripts and CI. The shorter interactive forms above
  work when task names do not collide with built-in commands, as explained in
  [mise's task documentation](https://mise.jdx.dev/tasks/running-tasks.html).
- [ ] Keep commands consistent between local use and CI. Separate checks that need
  network access or a live Hyprland/GPU session, and document their prerequisites.
  Keep end-user installation in the root installer, `install-service`, and package
  instructions; do not recreate the removed development install/uninstall tasks.

**Done when:** a clean checkout can run setup, check, and build through documented
tasks, and release preparation can be previewed without external writes.

## 4. Add quality checks and PR CI

- [ ] Resolve the formatting policy before enforcing it. Recommended: adopt
  rustfmt in a dedicated mechanical change, verify it, then replace the existing
  no-formatting instruction. Keep generated bindings under their generator's
  formatting policy. Do not mix formatting churn with behavior changes.
- [ ] Establish a small, useful linter baseline:
  - Rust: rustfmt and [Clippy](https://doc.rust-lang.org/clippy/usage.html) across
    maintained targets, with warnings treated as failures after fixing the baseline.
  - Shell: [ShellCheck](https://github.com/koalaman/shellcheck), including `install.sh`
    and applicable helper scripts.
  - GitHub Actions: [actionlint](https://github.com/rhysd/actionlint), or its maintained
    successor if appropriate when implemented.
  - Dependencies: [cargo-deny](https://embarkstudios.github.io/cargo-deny/) with an
    intentional license/advisory policy and narrowly documented exceptions.
  Avoid blanket pedantic rules, broad suppressions in handwritten code, and
  duplicate tools that report the same problems.
- [ ] Add PR and `main` CI that invokes the mise tasks. Linux checks must compile
  the capture, input, and encoding paths; macOS tests cover the portable code and
  do not substitute for Linux coverage. Preserve ARM64 release testing.
- [ ] Give required jobs stable names, run them on every PR, and cancel superseded
  runs. Avoid path filters that leave a required check permanently pending.
- [ ] Use minimal workflow permissions and reviewed, pinned action revisions.
  Keep release credentials out of PR execution. Enable dependency update alerts
  and group routine updates so maintenance stays manageable.
- [ ] Verify the advertised Rust minimum of 1.89 with locked dependencies, or
  deliberately update `rust-version` and the docs to the tested minimum.
- [ ] Review tests around authentication, protocol parsing, reconnects, clipboard,
  frame handling, and resource cleanup. Add tests for concrete gaps found during
  review, especially failure cases; avoid coverage quotas and tests that repeat
  the implementation.
- [ ] Document a short live smoke test on Hyprland with macOS Screen Sharing:
  connect, resize and restore, keyboard/mouse, clipboard both ways, sound,
  reconnect, and applicable VA-API/x265 paths. Record the tested setup and result.

**Done when:** a deliberately broken check blocks a PR, Linux code is exercised,
and the documented local commands reproduce CI's checks.

## 5. Generate release notes people want to read

Use [mise's git-cliff configuration](https://github.com/jdx/mise/blob/main/cliff.toml)
as a tooling reference for grouping Conventional Commits and linking PRs. For the
reading experience, use [uv's releases](https://github.com/astral-sh/uv/releases)
as a reference for focused sections and upgrade guidance, and
[mise's release presentation](https://github.com/jdx/mise/releases/tag/v2026.10.11)
for leading with the changes users will notice. Adapt these patterns to wayshare's
size; a small release should have short notes.

- [ ] Use a pinned [git-cliff](https://git-cliff.org/docs/) configuration as the
  single note generator, exposed through mise. Generate the changelog section
  during release preparation; use that same reviewed section in the GitHub draft.
  Fetch the needed history and tags in CI so the release range is complete.
- [ ] Generate entries from the final Conventional Commits, using descriptions
  already written for users. Strip commit syntax from display text, group changes,
  and link to their PR or commit. Deduplicate entries and omit empty sections.
- [ ] Present sections in this order when relevant: Breaking changes, Features,
  Fixes, Performance, Documentation, and Maintenance. Keep routine tooling and
  dependency churn out of the main user-facing sections. Never hide a security
  fix or breaking change because its commit type would normally be omitted.
- [ ] Put upgrade instructions beside breaking changes. Add a brief introductory
  highlight only when the release needs one. Include a full comparison link and
  contributor credit when available. Use restrained headings and whitespace;
  avoid decorative emoji, badges, and repeated boilerplate.
- [ ] Keep the current install and verification instructions concise and accurate.
  Preserve both architecture archives, checksums, and provenance attestations.
  Preserve the versionless asset names expected by `install.sh` and mise. Keep
  binary builds on the documented glibc baseline and verify the final x86_64
  artifact on Arch. Changes to `ubuntu-latest` for orchestration jobs do not
  justify changing the Ubuntu 22.04 binary-build runners.
- [ ] Make the notes reflect the artifacts actually produced. For a public release
  that promises attestations, fail release preparation if attestation generation
  fails, and verify each downloaded archive before publication. Never publish
  the current draft's unsupported attestation claim unchanged.
- [ ] Keep changelog generation deterministic. Run `$unslop` on the release-note
  templates when they change and on each release's generated prose before human
  approval. Save the reviewed wording in the changelog section used by the GitHub
  draft. Check correctness, omissions, and upgrade guidance before publication;
  do not overwrite reviewed wording silently when preparation is rerun.
- [ ] Extend the existing draft-release workflow rather than adding a parallel
  release system. Release preparation updates `Cargo.toml`, `Cargo.lock` when
  needed, and `CHANGELOG.md` in a PR. Tag only its merged `main` commit after checks
  pass; validate the version and tag, and reject an existing tag. Publishing the
  resulting draft remains an explicit maintainer action.
- [ ] Document the first-release boundary, prerelease handling, failed-run recovery,
  and the preparation/tag/publication sequence in `docs/releasing.md`. Do not
  rewrite published tags to retry a release.
- [ ] Add a small recovery path for an existing draft: an explicit manual trigger
  that builds the selected existing tag at its verified SHA and creates or updates
  that draft without changing the tag or overwriting a published release. Handle
  repeat asset uploads deliberately; the current unconditional `gh release create`
  will conflict with an existing draft.
  Use a fresh event after the visibility change to rebuild `v0.1.0` with
  attestations. The existing guard reads `github.event.repository.private`; do
  not assume rerunning the original private event refreshes that value. GitHub
  also preserves the original SHA and ref on a rerun; see
  [workflow rerun behavior](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/re-run-workflows-and-jobs).
- [ ] Confirm the changelog date during final preparation. If it should reflect
  publication rather than preparation, settle it before selecting the final
  tagged commit. Keep the notes consistent with the source being shipped.
  Leave binary stripping unchanged for launch; measure the size/debugging tradeoff
  separately if the reported roughly 2.7 MB archives become a practical problem.

**Done when:** previewing a representative release produces readable Markdown with
the right range, categories, links, and breaking-change instructions. Repeat the
preview to check stability, then inspect the rendered GitHub draft before publishing.

## 6. Measure builds and review code boundaries

- [ ] Measure cold builds, unchanged warm builds, and rebuilds after a small edit
  in both portable and Linux-specific code. Record the machine, command, profile,
  and timings in `docs/development.md` so future changes can be compared.
- [ ] Use Cargo's existing incremental development builds and preserve `target`
  between local runs. Keep release optimization separate. Cargo enables
  incremental compilation by default for development, but not release builds;
  see the [Cargo profile defaults](https://doc.rust-lang.org/cargo/reference/profiles.html).
- [ ] Cache dependencies and appropriate build outputs in CI, keyed by platform,
  toolchain, lockfile, and relevant build configuration. Check that restored
  caches improve timings. Avoid redundant Cargo work and competing jobs sharing
  a target directory. Add compiler caching only if measurements justify it.
- [ ] Review the existing module boundaries and large handwritten files, especially
  `rfb/session.rs`, `server_linux.rs`, and the VA-API implementation. Split modules
  when doing so clarifies responsibilities; line count alone is not a reason.
- [ ] Evaluate a Cargo workspace only if build measurements or dependency boundaries
  support it. Possible seams are portable protocol code and Linux platform code.
  Extract the smallest useful crate if it reduces coupling or rebuild cost.
  Otherwise retain the current package and record why. Keep generated bindings
  out of handwritten-code size judgments.
- [ ] Review broad dependency features, unnecessary public APIs, duplicated logic,
  unsafe boundaries, and error handling where they create a concrete maintenance
  problem. Make focused changes with relevant verification. Do not turn this
  review into a framework rewrite.

**Done when:** build behavior is measured and documented, and the crate-layout
decision has a concrete reason that another maintainer can assess.

## 7. Configure the public repository

Apply these settings after CI exists and before announcing the project. Audit
the live settings first, then read them back after changes.

- [ ] Keep Issues enabled and set PR creation to **Collaborators only**. Here,
  "contributors" means people Wes has explicitly invited to collaborate;
  contributing a past commit does not itself grant that access. GitHub supports
  this setting directly; see [restricting pull requests](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/enabling-features-for-your-repository/disabling-pull-requests).
  Explain in `CONTRIBUTING.md` that everyone can report issues and propose work,
  and that PR access is granted deliberately.
- [ ] Disable Discussions. Disable the wiki if the existing repository docs cover
  its purpose. Use Issues as the public route for bugs and proposals.
- [ ] Add bug-report and feature-request templates and disable blank issues.
  Ask for reproducible steps and expected behavior. Bug reports should capture
  wayshare, Hyprland, and macOS versions, GPU/driver, encoding mode, install method,
  and relevant redacted logs. Keep required fields limited to useful information.
- [ ] Add `SECURITY.md` with a verified private reporting route and enable private
  vulnerability reporting where available. Direct sensitive reports away from
  public issues. Check tracked files and history for secrets and private captures
  before changing visibility.
- [ ] Create an active `main` ruleset: PRs required, required CI checks, resolved
  review conversations, blocked force pushes and branch deletion, and no routine
  direct-push bypass for maintainers. Use linear history with squash merges.
  Start with zero required approvals if Wes is the only reviewer; require one
  independent approval once a second active maintainer exists. GitHub can require
  PRs without requiring approval; see [ruleset options](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets).
- [ ] Verify ruleset availability for the repository's plan and visibility. If
  protection cannot be enforced while private, prepare it beforehand and activate
  and verify it immediately after the visibility change, before accepting work.
  Check that `v*` tag protections permit the documented release path while blocking
  published-tag changes and deletion. Enable automatic merged-branch deletion.
- [ ] Use a small label set with descriptions: `bug`, `enhancement`, `documentation`,
  `needs-info`, `blocked`, `duplicate`, and `wontfix`. Add `help wanted` and
  `good first issue` only when scoped work and an invitation path exist.
- [ ] Set the repository description from the README's purpose. Add accurate topics:
  `rust`, `wayland`, `hyprland`, `linux`, `macos`, `screen-sharing`, `remote-desktop`,
  `vnc`, and `hevc`. Keep release tags consistently `v<semver>`.
- [ ] Add `.github/FUNDING.yml` matching Omastorm:

  ```yaml
  github: wesleygrimes
  buy_me_a_coffee: ncfrontiersman
  ```

  Enable the sponsorship button and add a modest README support link. Verify both
  destinations and the public button rather than assuming the file is sufficient.

**Done when:** outsiders can report issues but cannot create PRs, Discussions is
off, `main` protections are active, and the public metadata and funding links work.

## 8. Finish the identity and launch

- [ ] Design a simple, original logo/icon that suggests sharing a desktop. Start
  with a few small concepts, choose one, and keep the final mark readable at
  16–32 px and in one color. Avoid fine detail and dependence on Apple branding.
- [ ] Store an editable SVG master and only the raster exports actually needed.
  Verify it on light and dark backgrounds. Use it consistently in the README
  and a simple GitHub social preview; skip a larger branding system.
- [ ] Audit README promises against what exists. Confirm release downloads and
  installer behavior, and verify AUR/Omarchy availability before advertising those
  install routes. Check links, supported platforms, prerequisites, and limitations.
- [ ] Verify setup, checks, build, and release preview from a fresh checkout.
  Complete the live smoke test and review the generated release draft.
- [ ] Complete the public-install launch sequence, with authorization for each
  external action unless already explicitly requested:
  1. Finish the repository and release preparation, including draft recovery.
  2. Change visibility and verify the public view, enforceable rules, issue forms,
     PR restriction, and funding button.
  3. Build the chosen release tag through a fresh public workflow event. For the
     existing `v0.1.0` draft, use the recovery path above rather than deleting and
     recreating the tag just to trigger another build.
  4. Inspect the draft, download both archives, verify their checksums and
     attestations, and confirm notes and assets match the selected source commit.
  5. Publish the reviewed release as a stable release so the latest-release URLs
     can resolve it.
  6. From a machine without repository credentials, run the README's actual curl
     installer and a real `mise use -g github:wesleygrimes/wayshare` installation.
     Verify `--version`, service setup/startup, and an upgrade through each path.
     Recheck checksum rejection against the final installer. Record ARM64 runtime
     coverage honestly if no ARM machine is available.
  7. Announce after these checks pass. Local copies of release assets do not
     establish that anonymous GitHub downloads work.

## Suggested implementation order

1. Working agreements, contribution docs, and Conventional Commits.
2. Formatting decision, mise tasks, linter baseline, and PR CI.
3. Generated release notes and the release dry run.
4. Build measurements and any justified module/crate changes.
5. GitHub settings, templates, funding, and metadata.
6. Logo, fresh-install verification, and the public launch.

Keep each change reviewable. A measured decision to keep the current crate layout
is a valid result. Additional architecture, automation, and policy should earn
their place by making this project easier for a human to maintain.
