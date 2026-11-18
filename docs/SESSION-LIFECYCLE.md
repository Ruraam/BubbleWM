# BubbleSession lifecycle and systemd containment

## Status

The root-only `bubblewm-session` supervisor now starts a dedicated
`bubblewm-session-worker`. The worker takes ownership of fd 3 first, sends
the fixed `BUBWORK1` marker, reads the password directly from inherited stdin,
opens PAM, sends one bounded `SessionReady` frame, and waits for an explicit
`BUBOKAY1` approval from the supervisor before starting the fixed compositor
as the user. The worker's approval wait is bounded to 30 seconds; timeout,
EOF, or an invalid marker aborts the session and triggers PAM cleanup. The
supervisor never reads or buffers the password; it resolves the worker PID
through logind, compares the result with the worker's metadata, and only then
sends approval. After the desktop process group exits, the worker closes PAM.

The separation of PAM ownership and the independent logind identity check are
implemented. The worker binary is expected at
`/usr/libexec/bubblewm-session-worker`. Supervisor-owned systemd scope-stop cleanup is now implemented: after the
worker exits and PAM has closed, the supervisor requests `StopUnit` for the
independently resolved scope and waits until `ActiveState` becomes `inactive`.
The packaging staging target now includes the Debian PAM profile and the
root-managed session configuration, while refusing to overwrite an existing
live PAM service during a direct install. A real-PAM/logind probe and disposable
Debian 13 VM runner are available under `tests/systemd-vm`. The VM harness now
also exercises invalid-password rejection and escaped-process cleanup under
both effective `KillUserProcesses` policies: logind-owned cleanup when enabled,
and explicit supervisor-owned scope cleanup when disabled. The harness checks
the merged logind configuration and rejects policy overrides that would make
the test ambiguous. It has not yet been executed in a real Debian VM; greeter/VT handoff,
full compositor startup, and supervisor crash-recovery tests remain
outstanding.

## Ownership boundaries

BubbleSession must keep three responsibilities separate:

1. **System supervisor (root, outside the user session cgroup):** owns the
   greeter-facing protocol, starts and monitors one session worker, retains the
   logind session/scope identity, and performs final cgroup cleanup.
2. **Session worker (root, dedicated and single-threaded):** owns the PAM
   handle and supplementary-group transition. It authenticates the selected
   account, opens the PAM session, starts the fixed compositor command as the
   user, waits for it, performs ordinary process-group cleanup, closes PAM,
   and exits.
3. **Desktop session (unprivileged user):** BubbleWM and all applications
   launched by it. The compositor must never run as root.

The worker must remain single-threaded while `PamLoginSession` is alive because
opening the session changes supplementary groups process-wide. The supervisor
must not perform that transition in its own process.

## Session registration: PAM and logind

On systemd systems, the reviewed `bubblewm` PAM service must include the
distribution's normal session stack and ensure `pam_systemd.so` runs exactly
once. Do not call logind's `CreateSession` or `ReleaseSession` directly:
PAM/`pam_systemd` owns registration and release.

When the PAM session opens, `pam_systemd` registers the login with
systemd-logind, creates the session scope, establishes
`XDG_SESSION_ID` and `XDG_RUNTIME_DIR`, and associates the session with the
requested seat/VT when that metadata is supplied. The worker must set the
session type to `wayland`, set the desktop identity to `BubbleWM`, and pass
the selected VT as PAM_TTY plus the corresponding VT metadata before
`pam_open_session`.

The compositor is spawned by the worker after PAM session setup. It therefore
inherits the session cgroup and PAM-provided runtime/session environment.
BubbleWM's native DRM backend already uses libseat and libinput; libseat must
acquire the seat for this registered logind session rather than inventing a
second device-ownership path.

The Debian profile currently includes the host `login` PAM stack. A distro
package must verify that this stack actually invokes `pam_systemd.so`; the
presence of `@include login` alone is not a portable guarantee across
distributions or local PAM policy. A systemd-backed production login must fail
closed if it cannot obtain a real logind session, session scope, and usable
runtime directory. A non-systemd backend needs its own explicitly designed
session/cgroup implementation; it must not silently claim equivalent
containment.

## Why the current process-group cleanup is insufficient

The existing `ManagedUserProcess` sends signals to the compositor's process
group. A descendant can create a new session or process group and escape those
signals while remaining in the logind session cgroup.

The process group remains useful for graceful logout and as a fallback, but
it is not the authoritative session boundary. The session scope cgroup is.

## Required supervisor/worker protocol

The production design must keep the supervisor outside the user session scope:

1. The root supervisor creates a connected Unix socket pair with
   `FD_CLOEXEC` enforced on both endpoints, keeps one endpoint private, and
   starts a dedicated worker with the other endpoint through
   `spawn_worker_with_channel`. The worker receives the socket on fixed
   descriptor 3, without advertising the descriptor in argv or environment
   variables. At process startup, before creating threads, it must call
   `take_worker_channel`; that validates a connected Unix stream socket and
   sets `FD_CLOEXEC` again to prevent the channel leaking into later execs.
   The spawn primitive uses an empty environment. Immediately after taking
   ownership of fd 3, the single-threaded worker closes every descriptor from
   fd 4 upward with Linux `close_range`, falling back to enumeration through
   `/proc/self/fd` when the syscall is unavailable. This prevents inherited
   descriptors from reaching PAM handling or the compositor. Before trusting
   any later worker output, the supervisor must read and validate the exact
   fixed `BUBWORK1` bootstrap marker; EOF, truncation or a different marker is
   a worker-start failure.
2. The worker opens PAM. After `pam_open_session` succeeds, it sends one
   versioned `SessionReady` frame over the private control stream. The bounded
   frame contains only the numeric UID, opaque `XDG_SESSION_ID`, validated
   `XDG_RUNTIME_DIR`, optional seat name and optional VT number. The protocol
   has no password, command, object-path or systemd-unit field. Passwords
   never cross this channel.
3. The supervisor independently resolves the session and scope from the
   worker PID using the libsystemd `sd-login` APIs. It validates the UID,
   session ID, scope name and runtime directory against the worker's
   `SessionReady` frame. The worker must never supply a unit name or object
   path for the supervisor to trust. The supervisor retains the validated scope name and uses the systemd unit
   manager for final cleanup only after the worker has exited.
4. The worker starts the fixed compositor as the authenticated user, waits for
   it, performs process-group cleanup, closes PAM, and exits. Startup failure
   follows the same cleanup path.
5. The supervisor reaps the worker, then calls `StopUnit` for the retained
   scope through the system bus. It waits for the unit `ActiveState` to become
   `inactive`, with a bounded timeout. An already-inactive scope is treated as
   an idempotent success; missing units, D-Bus errors, failed state and timeout
   are reported as cleanup failures.
6. The supervisor reports logout complete only after the scope is confirmed
   inactive. This removes descendants that escaped the original process
   group, independently of the machine-wide `KillUserProcesses=` policy.

The supervisor must never ask logind to terminate the session while the PAM
worker is still expected to close its PAM handle: terminating the scope would
also kill that worker. The worker must not try to stop its own session scope,
because it belongs to that scope. The external supervisor owns final scope
cleanup.

Unit names and object paths are obtained from logind/systemd, not constructed
from the username, PID, or user-controlled input. D-Bus calls use the system
bus and are made only by the privileged supervisor. Errors, timeouts, missing
units, and already-stopped scopes need explicit, idempotent handling.

## VT and seat lifecycle

PAM registration and cgroup containment do not by themselves prove that the
requested VT is active or that the compositor owns the seat. Production
startup must validate the real tty device, provide the VT/seat metadata to
PAM, and let libseat acquire the DRM/input devices for the registered session.
The supervisor must coordinate greeter VT release, compositor seat acquisition,
VT activation, pause/resume, and recovery if startup fails midway.

BubbleWM's current libseat pause/resume handling is a backend primitive, not
yet a complete greeter-to-session handoff.

## Required validation

Before production use, extend the systemd VM integration tests to cover:

- successful PAM open and close with a real `pam_systemd` session;
- correct `XDG_SESSION_ID`, `XDG_RUNTIME_DIR`, seat and VT metadata;
- compositor and ordinary descendants placed in the same session scope;
- [x] a descendant that deliberately escapes its process group stopped by
  supervisor-owned scope cleanup (VM harness implemented; runtime execution
  still pending);
- compositor crash, failed exec, failed PAM setup, worker crash and supervisor
  restart;
- logout when `KillUserProcesses=yes` and when it is `no`;
- no leaked scope, runtime directory, device access, or stale greeter state;
- seat/VT handoff and recovery on real virtual terminals.

Unit tests with mocked D-Bus responses are useful, but they do not replace the
VM tests: the cgroup and logind behavior must be exercised against a real
systemd instance.
