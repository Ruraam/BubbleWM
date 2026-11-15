# BubbleSession lifecycle and systemd containment

## Status

Design note for the production login/session supervisor. The current
`bubblewm-session` binary is still a root-only prototype helper. A dedicated
`bubblewm-session-worker` bootstrap executable now takes ownership of fd 3
immediately, validates the private Unix stream, sends the fixed `BUBWORK1`
bootstrap marker, and remains alive until the supervisor closes the channel.
This is only a transport/bootstrap milestone: it does not authenticate, open
PAM, launch the compositor, or implement a production shutdown protocol. The
bounded metadata protocol and low-level `posix_spawn` handoff primitives are
implemented and unit-tested; supervisor ownership and systemd unit cleanup
are not yet wired together.

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
   The spawn primitive uses an empty environment and does not yet enforce a
   general close-all-unrelated-descriptors policy, so the production
   supervisor must retain a strict descriptor hygiene policy. Before trusting
   any later worker output, the supervisor must read and validate the exact
   fixed `BUBWORK1` bootstrap marker; EOF, truncation or a different marker is
   a worker-start failure.
2. The worker opens PAM. After `pam_open_session` succeeds, it sends one
   versioned `SessionReady` frame over the private control stream. The bounded
   frame contains only the numeric UID, opaque `XDG_SESSION_ID`, validated
   `XDG_RUNTIME_DIR`, optional seat name and optional VT number. The protocol
   has no password, command, object-path or systemd-unit field. Passwords
   never cross this channel.
3. The supervisor resolves the logind session object and systemd scope from
   the received session ID using the system bus. It retains the unit name while
   the logind session object is present and validates that the unit is a
   session scope associated with the expected UID and session. The worker must
   never supply a unit name or object path for the supervisor to trust.
4. The worker starts the fixed compositor as the authenticated user, waits for
   it, performs process-group cleanup, closes PAM, and exits. Startup failure
   follows the same cleanup path.
5. The supervisor reaps the worker and explicitly stops the retained session
   scope through the systemd unit manager, waiting for the stop job to finish.
   This final step removes descendants that escaped the original process
   group, independently of the machine-wide `KillUserProcesses=` policy.
6. The supervisor verifies that the scope is inactive before reporting logout
   complete or starting another login on the same seat.

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

Before production use, add systemd VM integration tests covering:

- successful PAM open and close with a real `pam_systemd` session;
- correct `XDG_SESSION_ID`, `XDG_RUNTIME_DIR`, seat and VT metadata;
- compositor and ordinary descendants placed in the same session scope;
- a descendant that deliberately escapes its process group still stopped by
  supervisor-owned scope cleanup;
- compositor crash, failed exec, failed PAM setup, worker crash and supervisor
  restart;
- logout when `KillUserProcesses=yes` and when it is `no`;
- no leaked scope, runtime directory, device access, or stale greeter state;
- seat/VT handoff and recovery on real virtual terminals.

Unit tests with mocked D-Bus responses are useful, but they do not replace the
VM tests: the cgroup and logind behavior must be exercised against a real
systemd instance.
