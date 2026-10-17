# Working agreements

Rules for anyone, human or agent, changing wayshare. Setup and build instructions are in
[docs/development.md](docs/development.md). The release process is in [docs/releasing.md](docs/releasing.md).

## How to work

- Work inline unless Wes explicitly asks for delegation.
- Keep changes within the request. Describe nontrivial edits before making them.
- Prefer simple functions, clear ownership, and existing patterns. Add an abstraction or
  dependency only when it solves a concrete problem.
- Comment on intent, protocol constraints, and safety requirements. Skip comments that
  narrate obvious code. Explain the invariants around unsafe FFI.
- Change generated files through their generator (for example `mise run gen-vaapi-sys`),
  not by hand.
- Use `wes/<task-name>` branches and the mise tasks. Write commit messages and PR titles as
  Conventional Commits; CI fails the PR otherwise (see [CONTRIBUTING.md](CONTRIBUTING.md)).
- Run the checks that fit the change and report what actually ran, including checks that
  could not run on the available machine.
- Get authorization before external writes (pushes, PRs, releases, repository settings)
  unless the action was explicitly requested.

## Writing

All project writing is for humans; no AI slop. This covers docs, PR descriptions, commit
messages, issue text, and release notes.

- Lead with the concrete behavior, problem, or benefit.
- Use plain words and short, specific sentences. Explain why when it helps.
- PR descriptions state what changed, why, and how it was verified. Scale the detail to
  the change.
- Release notes explain what users will notice and any upgrade action.
- Cut hype, filler, canned summaries, and tool branding. Never invent
  test results, performance gains, or compatibility claims.
- Review generated text as carefully as code. A linter can't tell you whether writing is
  useful.

## Required `$unslop` review

Run `$unslop` on every new or changed document or template before handoff or publication:
the README, this file, contributor and security guides, `docs/`, issue and PR templates,
release-note templates, and generated release notes.

Read the skill's `SKILL.md`, scan for its patterns, revise, and self-audit. Keep technical
meaning, commands, links, and template syntax intact, and check they still work. Report
which files got the pass. If the skill is unavailable, say so instead of calling the
review done.
