def _frozen_validation_authority_commit(config: Config) -> tuple[Path, str]:
    """Bind parser authority through both enclosing, pinned repository Gitlinks."""

    authority, consumer, agent_checkout = _frozen_validation_authority_chain(config)
    agent_relative = agent_checkout.relative_to(consumer).as_posix()
    consumer_relative = consumer.relative_to(authority).as_posix()

    agent_head = _trusted_git_head(agent_checkout, "agent-utils HEAD")
    consumer_head = _trusted_git_head(consumer, "enclosing consumer HEAD")
    if (
        _trusted_gitlink(
            consumer,
            consumer_head,
            agent_relative,
            label="enclosing consumer agent-utils Gitlink",
        )
        != agent_head
    ):
        raise Refusal(
            "loaded agent-utils HEAD differs from the enclosing checkout's pinned Gitlink"
        )
    authority_head = _trusted_git_head(authority, "outer authority HEAD")
    if (
        _trusted_gitlink(
            authority,
            authority_head,
            consumer_relative,
            label="outer authority consumer Gitlink",
        )
        != consumer_head
    ):
        raise Refusal(
            "enclosing checkout HEAD differs from the outer authority's pinned Gitlink"
        )
    authority_common = _trusted_git_directory(
        authority,
        ("rev-parse", "--path-format=absolute", "--git-common-dir"),
        label="outer authority common Git directory",
    )
    config_common = _trusted_git_directory(
        config.root,
        ("rev-parse", "--path-format=absolute", "--git-common-dir"),
        label="frozen validation recovery common Git directory",
    )
    if config_common != authority_common:
        try:
            _authenticate_frozen_tool_state_root(
                config, authority, consumer, agent_checkout,
                authority_head, consumer_head, agent_head,
            )
        except Refusal as exc:
            raise Refusal(
                "frozen validation recovery project root does not share the authenticated "
                "outer authority Git repository; sealed separate-clone authority refused: "
                f"{exc}"
            ) from exc
    return authority, authority_head
