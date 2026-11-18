# Real PAM + systemd-logind integration test (Debian 13)

This test is intentionally destructive to a disposable VM only: it installs
build dependencies, adds and removes a temporary local account, and installs
`/etc/pam.d/bubblewm` only when that service file does not already exist.
Do not run it on a workstation or production login host.

## VM requirements

- Debian 13, booted normally with systemd as PID 1;
- active `systemd-logind.service`;
- a local console/VT (the test sets PAM_TTY to `/dev/tty1`);
- root access and network access for apt.

Take a VM snapshot first. Log in on the VM's local console as root, then run:

```sh
git clone https://github.com/Ruraam/BubbleWM.git
cd BubbleWM
git checkout codex/bubblewm-visual-theme-engine
sudo sh tests/systemd-vm/run-debian.sh
```

The harness verifies that Debian's `common-session` contains exactly one
`pam_systemd.so` entry, compiles a small C probe against libpam/libsystemd,
creates a disposable unprivileged account, and exercises the actual
`bubblewm` PAM service:

1. PAM authentication and account policy;
2. `pam_open_session` and PAM-provided `XDG_SESSION_ID` /
   `XDG_RUNTIME_DIR`;
3. independent logind resolution of the probe PID, UID and session scope;
4. an active `session-*.scope` while PAM is open;
5. `pam_close_session` / `pam_end` and scope deactivation.

The password is generated for the temporary account and passed to the probe
through stdin, never through argv. The account and temporary files are removed
on exit.

This test validates PAM/logind registration and release. It does **not** yet
validate the full BubbleWM compositor startup, greeter handoff, DRM seat
acquisition, escaped-descendant cleanup, or supervisor crash recovery. Those
require separate VM scenarios and must not be inferred from this probe.
