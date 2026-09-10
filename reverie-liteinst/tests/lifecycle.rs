use std::io;

use reverie::Backend;
use reverie::Error;
use reverie::process::Command;
use reverie_liteinst::LiteinstBackend;

fn assert_unsupported(error: Error) {
    match error {
        Error::Io(error) => assert_eq!(error.kind(), io::ErrorKind::Unsupported),
        error => panic!("expected an I/O Unsupported error, got {error:?}"),
    }
}

#[tokio::test]
async fn backend_trait_entry_points_refuse_before_program_lookup() {
    let command = || Command::new("/definitely-not-a-program");

    assert_unsupported(
        <LiteinstBackend as Backend>::run::<()>(command(), ())
            .await
            .unwrap_err(),
    );
    assert_unsupported(
        <LiteinstBackend as Backend>::run_with_stats::<()>(command(), ())
            .await
            .unwrap_err(),
    );
    assert_unsupported(
        <LiteinstBackend as Backend>::run_with_output::<()>(command(), ())
            .await
            .unwrap_err(),
    );
}
