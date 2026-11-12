def _frozen_validation_authority_chain(config: Config) -> tuple[Path, Path, Path]:
    """Return the authenticated outer, consumer, and agent-utils checkouts."""

    loaded = Path(__file__).resolve(strict=True)
    agent_checkout = _trusted_git_toplevel(
        loaded.parent, label="loaded agent-utils checkout"
    )
    try:
        loaded.relative_to(agent_checkout)
    except ValueError as exc:
        raise Refusal("loaded wrkslots module is outside its authenticated checkout") from exc
    consumer = _trusted_git_superproject(
        agent_checkout, label="agent-utils"
    )
    authority = _trusted_git_superproject(
        consumer, label="consumer"
    )
    config_root = _trusted_git_toplevel(
        config.root,
        label="frozen validation recovery project root",
    )
    if config_root != config.root.resolve():
        raise Refusal("frozen validation recovery project root is not a Git worktree root")
    try:
        agent_relative = agent_checkout.relative_to(consumer).as_posix()
        consumer_relative = consumer.relative_to(authority).as_posix()
    except ValueError as exc:
        raise Refusal("authenticated submodule checkout is outside its superproject") from exc
    if agent_relative in {"", "."} or consumer_relative in {"", "."}:
        raise Refusal("authenticated submodule checkout has an invalid Gitlink path")
    return authority, consumer, agent_checkout
