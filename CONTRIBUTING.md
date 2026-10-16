# Contributing

## Issues

Anyone can report a bug or propose a feature. Use the issue forms and include what you
ran, what you expected, and what happened. Redact passwords, hostnames, and IP addresses
from logs. Report security problems privately; see [SECURITY.md](SECURITY.md).

## Pull requests

Pull requests are limited to collaborators. Open an issue first to propose work. PR access
is granted deliberately, not by having a past commit.

## Branches and commits

- Branch as `wes/<task-name>`.
- Write commits and PR titles as `<type>(optional-scope): <description>` following
  [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).
- Types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`,
  `style`, `revert`. Add a scope only when it clarifies the change.
- Mark breaking changes with `!` or a `BREAKING CHANGE:` footer, and say who is affected
  and what they need to do.
- Below 1.0, the maintainer picks each release version explicitly.

```text
fix(clipboard): preserve copied images when reconnecting
perf(encode): reuse buffers between video frames
docs: explain which firewall ports to open
```

PRs are squash-merged and the PR title becomes the commit message, so write the title
for the changelog reader. For a breaking change, keep the `BREAKING CHANGE:` footer and
upgrade steps in the squash commit body.

## Setup and checks

Run `mise run check` before pushing. Setup, prerequisites, and the other tasks are in
[docs/development.md](docs/development.md).

## Review

State the problem, the result, and how you verified it. Keep PRs small and within one
purpose. Say which checks you could not run. Writing follows the rules in
[AGENTS.md](AGENTS.md), including the `$unslop` pass for docs and templates.
