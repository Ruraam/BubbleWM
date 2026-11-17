//! Process-level tests for the dedicated session worker bootstrap.

use std::ffi::OsStr;

use bubblewm::session_control::{
    private_channel_pair, read_worker_hello, spawn_worker_with_channel,
};

#[test]
fn worker_handshakes_before_rejecting_an_invalid_invocation() {
    let (mut supervisor, worker_channel) = private_channel_pair().unwrap();
    let worker_path = OsStr::new(env!("CARGO_BIN_EXE_bubblewm-session-worker"));

    let mut worker =
        spawn_worker_with_channel(worker_path, &[], worker_channel).expect("spawn session worker");

    read_worker_hello(&mut supervisor).expect("worker bootstrap handshake");

    // The worker now requires an account and VT before entering the PAM
    // lifecycle. This bootstrap-only invocation intentionally omits them;
    // the test verifies the handshake happens before argument validation.
    drop(supervisor);

    let exit_status = worker.wait().expect("reap session worker");
    assert_eq!(
        exit_status.code(),
        Some(1),
        "worker should reject the incomplete invocation: {exit_status:?}"
    );
}
