/// Failed spawns transfer their unpolled consuming cleanup out of the parent
/// callback. Its driver publishes the parent failure before calling this;
/// a bare RunAborted is the child's derived failure-cleanup marker only.
async fn finish_unstarted_tool_cleanups(executor: &mut ElfExecutor) -> Result<()> {
    let mut errors = Vec::new();
    for cleanup in executor.take_unstarted_tool_cleanup() {
        match cleanup.await {
            Ok(()) | Err(Error::RunAborted) => {}
            Err(error) => errors.push(error),
        }
    }
    Error::combine(errors)
}
