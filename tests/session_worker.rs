//! Process-level tests for the dedicated session worker bootstrap.

use std::ffi::OsStr;
use std::os::unix::process::ExitStatusExt;

use bubblewm::session_control::{private_channel_pair, read_worker_hello, spawn_worker_with_channel};

#[test]
fn worker_handshakes_and_exits_after_supervisor_closes_channel() {
    let (mut supervisor, worker_channel) = private_channel_pair().unwrap();
    let worker_path = OsStr::new(env!("CARGO_BIN_EXE_bubblewm-session-worker"));

    let pid = spawn_worker_with_channel(worker_path, &[], worker_channel)
        .expect("spawn session worker");

    read_worker_hello(&mut supervisor).expect("worker bootstrap handshake");

    // EOF is the deliberately minimal bootstrap shutdown signal.
    drop(supervisor);

    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            break;
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("wait for session worker: {error}");
        }
    }

    assert!(
        libc::WIFEXITED(status),
        "worker terminated by signal {:?}",
        ExitStatusExt::from_raw(status)
    );
    assert_eq!(libc::WEXITSTATUS(status), 0);
}
