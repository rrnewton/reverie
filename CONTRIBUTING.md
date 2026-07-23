# Contributing to Reverie

We want to make contributing to this project as easy and transparent as
possible.

## Our Development Process

Reverie is currently developed in Meta's internal repositories and then
exported out to GitHub by a Meta team member; however, we invite you to
submit pull requests as described below.

## Pull Requests

We actively welcome your pull requests.

1. Fork the repo and create your branch from `main`.
2. If you've added code that should be tested, add tests.
3. If you've changed APIs, update the documentation.
4. Ensure the test suite passes.
5. Make sure your code lints.
6. If you haven't already, complete the Contributor License Agreement ("CLA").

## Contributor License Agreement ("CLA")

In order to accept your pull request, we need you to submit a CLA. You only
need to do this once to work on any of Meta's open source projects.

Complete your CLA here: <https://code.facebook.com/cla>

## Issues

We use GitHub issues to track public bugs. Please ensure your description is
clear and has sufficient instructions to be able to reproduce the issue.

Meta has a [bounty program](https://www.facebook.com/whitehat/) for the safe
disclosure of security bugs. In those cases, please go through the process
outlined on that page and do not file a public issue.

## Coding Style

Follow the automatic `rustfmt` configuration.

## Tracking Stubs

Intermediate, intentionally-incomplete code is acceptable, but it must be
visible and tracked so completeness can be judged. Every stub — an
`unimplemented!()`/`todo!()`, a placeholder that returns a canned value, or a
method/crate that does not yet fulfill a contract it advertises — must carry a
marker of the form:

```rust
// TODO-STUB(#<issue>): <brief description of what needs implementing>
```

where `#<issue>` references a tracking issue on
[`rrnewton/reverie`](https://github.com/rrnewton/reverie/issues). Open the
issue first, then annotate the code so the stub is greppable and linked to its
plan.

Enumerate and count the markers with:

```bash
scripts/count-stubs.sh          # list every TODO-STUB with file:line, then total
scripts/count-stubs.sh --count  # print only the total count
```

The count should trend toward zero over time; new stubs are fine only when they
come with an issue and a marker.

## License

By contributing to Reverie, you agree that your contributions will be
licensed under the LICENSE file in the root directory of this source tree.
