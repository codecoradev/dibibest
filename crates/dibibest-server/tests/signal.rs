use dibibest_server::signal_or_never;
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn a_signal_ends_the_wait() {
    let done = timeout(
        Duration::from_secs(2),
        signal_or_never("test", async { Ok(()) }),
    )
    .await;
    assert!(done.is_ok(), "a signal must complete the future");
}

#[tokio::test]
async fn a_failed_signal_handler_is_not_a_shutdown_request() {
    let done = timeout(
        Duration::from_millis(200),
        signal_or_never("test", async {
            Err(std::io::Error::other("cannot register"))
        }),
    )
    .await;
    assert!(
        done.is_err(),
        "a handler that failed to register must wait forever, not stop the server"
    );
}
