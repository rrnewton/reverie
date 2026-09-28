def _trusted_git_text(
    repository: Path, arguments: Sequence[str], *, label: str
) -> str:
    raw = _trusted_git_capture(repository, arguments, label=label, stdout_limit=4096)
    try:
        value = raw.decode("ascii").strip()
    except UnicodeDecodeError as exc:
        raise Refusal(f"cannot authenticate {label}: Git returned non-ASCII data") from exc
    if not value or "\n" in value or "\r" in value:
        raise Refusal(f"cannot authenticate {label}: Git returned an invalid value")
    return value
