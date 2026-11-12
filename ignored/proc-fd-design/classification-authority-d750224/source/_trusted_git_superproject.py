def _trusted_git_superproject(repository: Path, *, label: str) -> Path:
    superproject = _trusted_git_directory(
        repository,
        (
            "rev-parse",
            "--path-format=absolute",
            "--show-superproject-working-tree",
        ),
        label=f"{label} superproject checkout",
    )
    if _trusted_git_toplevel(superproject, label=label) != superproject:
        raise Refusal(f"{label} superproject is not a Git worktree root")
    return superproject
