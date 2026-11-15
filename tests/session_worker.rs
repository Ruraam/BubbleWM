//! Process-level tests for the dedicated session worker bootstrap.

use std::ffi::OsStr;

use bubblewm::session_control::{
    private_channel_pair, read_worker_hello, spawn_worker_with_channel,
};

#[test]
fn worker_handshakes_and_exits_after_supervisor_closes_channel() {
    let (mut supervisor, worker_channel) = private_channel_pair().unwrap();
    let worker_path = OsStr::new(env!("CARGO_BIN_EXE_bubblewm-session-worker"));

    let mut worker =
        spawn_worker_with_channel(worker_path, &[], worker_channel).expect("spawn session worker");

    read_worker_hello(&mut supervisor).expect("worker bootstrap handshake");

    // EOF is the deliberately minimal bootstrap shutdown signal.
    drop(supervisor);

    let exit_status = worker.wait().expect("reap session worker");
    assert!(
        exit_status.success(),
        "worker did not exit successfully: {exit_status:?}"
    );
}
