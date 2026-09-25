# Contributing

Bug reports, feature requests and pull requests are all welcome. If something
about Luxel is wrong, awkward or missing, an issue describing it is worth as
much as a patch.

## Ways in

- **Report a bug:** [open an issue](https://github.com/hyprlab/luxel/issues).
  The version, your distribution and which devices are involved (LIFX model,
  SmartLife plug, local or cloud) narrow most problems down fast.
- **Suggest a feature:** an issue describing what you are trying to do, rather
  than the control you imagine, gets the best result.
- **Code:** send a pull request; see below.

## Pull requests

- Branch from `main` and keep the change to one subject.
- Build it and run it before opening the PR (`cargo build`, then the app).
- Match the surrounding code: comments explain *why*, not *what*.
- There's no CLA. By opening a pull request you agree your contribution ships
  under the [AGPL-3.0-or-later](../LICENSE), and that it may be adapted before
  it lands, with the change explained on the pull request.
- Contributions are credited by name and handle, so say if you would rather be
  credited differently, or not at all.

## Commits

Subjects follow [Conventional Commits](https://www.conventionalcommits.org):
`type(area): summary`, such as `fix(tuya): retry a plug that drops the session`
or `feat(scenes): export scenes to a file`. The type is one of `feat`, `fix`,
`perf`, `refactor`, `docs`, `build`, `ci`, `test`, `style`, `chore` or
`revert`; the area is the part of the app the change is in. Keep the subject
to 72 characters, lower case after the colon, with no full stop.

The body is optional. When there is one, it says in a few lines why the change
exists, in factual language with no marketing, no emoji and no em dashes, and
stays under 100 words. Write each paragraph as a single line: GitHub shows a
commit body with its line breaks kept, so a body wrapped by hand breaks again
wherever the screen is narrower than the wrap.

Trailers are for people: a `Co-Authored-By:` line credits somebody whose work
is in the commit, using the `users.noreply.github.com` address that resolves to
their profile, and not for the tools anyone wrote it with. The README's
[AI notice](../README.md#ai-notice) covers that for the repository as a whole,
so the history reads as the maintainer's own.
`tools/git-hooks/commit-msg` checks all of this; point a clone at the hooks with
`git config core.hooksPath tools/git-hooks`.

## AI-assisted contributions

Luxel is built with AI assistance itself (see the
[AI notice](../README.md#ai-notice)), so patches written with AI tools are as
welcome as any other. Everything merged gets the same human review, and the
same rule applies either way: you are responsible for what your patch does.

## Releases

Versions follow [Semantic Versioning 2.0.0](https://semver.org). Luxel is an
app, so its public interface is what users and their data depend on:

- **Major** (`2.0.0`): a change an existing install cannot carry over, such as
  a config format an older version cannot read, a removed feature or setting,
  a dropped device type or backend, or a new app ID.
- **Minor** (`1.3.0`): new functionality that keeps working with what is
  already there.
- **Patch** (`1.2.3`): bug fixes only.

Each release gets a section in [CHANGELOG.md](../CHANGELOG.md) (the detail)
and in [RELEASE_NOTES.md](../RELEASE_NOTES.md) (the overview the About window
shows). The Releases page and the tags keep the newest release of each minor
version.
