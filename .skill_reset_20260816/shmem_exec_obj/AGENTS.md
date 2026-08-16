# Shared-Memory Pod Project Guide

This guide applies only to `shmem_exec_obj/`. Do not modify the parent
workspace, `hermit/`, or `reverie/` while working on this project.

## Minibeads Tracking

Use this directory's minibeads repository for every nontrivial task.

1. Run `mb quickstart` before using the tracker.
2. Run `mb ready` and claim one ready issue with `mb claim <id>` before editing.
3. Record newly discovered work with `mb create`; add blocking relationships
   with `mb dep add` instead of burying follow-up work in prose.
4. Update issue notes and acceptance criteria through `mb update`. Never edit
   files under `.minibeads/issues/` by hand.
5. Close an issue only after its acceptance criteria and validation are met.
   Include the implementing commit and exact validation in the close reason or
   issue notes.
6. Commit minibead state with the source or documentation it describes. Do not
   leave completed work represented only in an uncommitted tracker file.

`pod-1` is the release epic. It remains open until every child is complete and
the source-blind release audit accepts the packaged library.

## Project Structure

- `latest` points to the current publishable iteration.
- `v1/` is preserved evidence; avoid changing it unless a regression requires
  repairing the original experiment.
- `v2/` is the current SDK, examples, private executable-image harness, and
  injection demonstrations.
- `ai_docs/` contains durable research, evidence, and review reports.

Public library code must remain usable without the private `poc/` and `demos/`
packages. Keep persistent shared-memory data pointer-free unless an API is
explicitly documented as fixed-address and experimental.

## Release Gate

Before claiming the release is complete, invoke the
`shmem-pod-blind-review` skill in `.llms/skills/`. Each retry must use a fresh
no-context reviewer. Blocking and major findings become minibeads issues and
must be fixed before another audit round.
