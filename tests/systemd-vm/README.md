# Real PAM + systemd-logind integration test (Debian 13)

This test is intentionally destructive to a disposable VM only: it installs
build dependencies, adds and removes a temporary local account, and installs
`/etc/pam.d/bubblewm` only when that service file does not already exist; it
refuses to proceed if the path is already occupied.
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
BUBBLEWM_EXPECT_KILL_USER_PROCESSES=yes sh tests/systemd-vm/run-debian.sh
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
5. rejection of an invalid password without reporting an opened session;
6. a child that calls `setsid()` and escapes the worker's process group while
   remaining in the logind session cgroup;
7. both effective `KillUserProcesses` policies:
   - `yes`: logind removes the escaped child and session scope during logout;
   - `no`: the child survives PAM close, then the supervisor's explicit
     `systemctl stop` must remove it;
8. scope deactivation and removal of the escaped child in either policy.

The password is generated for the temporary account and passed to the probe
through stdin, never through argv. The account and temporary files are removed
on exit.

## Running the policy matrix

Run the harness twice, from separate disposable VM snapshots or clean boots.
Set the policy in a logind drop-in before booting the VM; do not restart
`systemd-logind` from the active console session just to change the setting.

For the first boot, configure:

```ini
[Login]
KillUserProcesses=yes
```

Then run:

```sh
BUBBLEWM_EXPECT_KILL_USER_PROCESSES=yes sudo -E sh tests/systemd-vm/run-debian.sh
```

For the second boot, configure `KillUserProcesses=no` and run:

```sh
BUBBLEWM_EXPECT_KILL_USER_PROCESSES=no sh tests/systemd-vm/run-debian.sh
```

The harness reads the merged `systemd/logind.conf` configuration with
`systemd-analyze cat-config`, checks the expected value, and refuses ambiguous
`KillOnlyUsers` or test-user `KillExcludeUsers` overrides. The `yes` case
validates logind's automatic logout cleanup; the `no` case specifically
validates the supervisor's explicit scope-stop cleanup. Both must pass.

This test validates PAM/logind registration and cleanup of a descendant that
escaped its original process group under both supported logind policies. It
does **not** yet validate the full BubbleWM compositor startup, greeter/VT
handoff, DRM seat acquisition, or supervisor crash recovery. Those require
separate VM scenarios and must not be inferred from this probe.
