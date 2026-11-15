//! Dedicated, single-threaded BubbleWM session-worker bootstrap.
//!
//! This executable deliberately does not authenticate users or open PAM yet.
//! It validates the inherited private channel, announces its protocol identity,
//! and remains alive until the supervisor closes the channel.

use std::io::{self, Read, Write};

use bubblewm::session_control::take_worker_channel;

const WORKER_HELLO: &[u8; 8] = b"BUBWORK1";

fn main() {
    if let Err(error) = run() {
        eprintln!("bubblewm-session-worker: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    // This must remain the first operation: no argument parsing, logging setup,
    // thread creation, or other work may precede ownership of descriptor 3.
    let mut channel = take_worker_channel()?;
    announce_worker(&mut channel)?;

    // EOF is the current bootstrap shutdown signal. No lifecycle commands or
    // PAM integration are accepted by this deliberately narrow first version.
    io::copy(&mut channel, &mut io::sink())?;
    Ok(())
}

fn announce_worker<W: Write>(channel: &mut W) -> io::Result<()> {
    channel.write_all(WORKER_HELLO)?;
    channel.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn worker_announces_versioned_bootstrap_identity() {
        let (mut supervisor, mut worker) = UnixStream::pair().unwrap();
        announce_worker(&mut worker).unwrap();

        let mut hello = [0_u8; WORKER_HELLO.len()];
        supervisor.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, WORKER_HELLO);
    }
}
