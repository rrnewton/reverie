# 335ed55b gate run: RED, superseded

Commit 335ed55becd6302688ae9b68989db21682c7dff3 failed the exact-head pyre gate
(`pyre.rc` = 3). Two errors at `tests/test_prototype.py:1348` and `:1359`: a
`(run: Path) -> int` lambda was assigned to a `Callable[[Path], None]` dict value.
The pre-commit checks had not included pyre.

Gates run at 335ed55b before the failure:
- `commit-identity`: rc 0
- `sl-status-pre`: rc 0, empty
- `arc-lint-INVALID-nonexistent-path`: rc 1. INVALID: the command named a
  nonexistent `cli.py`, so it says nothing about lint.
- `arc-lint`: rc 0
- `pyre`: rc 3, RED

The fix widens the dict value type to `Callable[[Path], object]`, with no assertion
change, and is amended into a new head. All gates are re-run at that head in a
fresh directory. Nothing here is deleted or relabelled.
