# Session guardian design

## Decision

Use a small, privileged guardian process outside the logind user-session
scope. The guardian is started by `bubblewm-session` before the worker is
approved. It remains alive if the supervisor is terminated abruptly and
watches a private Unix socket owned by the supervisor.

A worker-side socket monitor is still required: it provides prompt PAM
cleanup and process-group termination. It is not the crash-recovery authority,
because a worker cannot reliably stop its own logind scope.

## Trust and ownership

- The supervisor remains the only component that resolves logind identity and
  chooses the scope to clean up.
- The guardian accepts a scope only over its private inherited control channel,
  after the supervisor has independently resolved and validated it.
- The guardian validates the scope syntax again. It never accepts a scope name
  from argv, the worker, an environment variable, or a user-facing request.
- The guardian runs as root and must remain outside the target
  `session-*.scope`. It must not inherit the worker's PAM handle, password
  descriptor, worker control socket, or arbitrary greeter descriptors.
- The worker continues to own PAM. Neither the supervisor nor guardian closes
  the worker's PAM session on its behalf.

## Arm and shutdown protocol

1. The supervisor starts the worker and receives its bounded metadata frame.
   It resolves the worker PID through libsystemd and verifies the exact
   `SessionIdentity` match.
2. Before sending approval, the supervisor opens a Linux pidfd for that exact
   worker process and starts the guardian using a separate private Unix
   socket. The pidfd is transferred with `SCM_RIGHTS`; a numeric PID alone
   is not an acceptable worker-liveness reference.
3. The supervisor sends one bounded arm message containing only the validated
   logind scope name. The guardian validates the message and scope, retains
   the pidfd, and acknowledges that it is armed.
4. Only after the guardian's acknowledgement does the supervisor send
   `BUBOKAY1` to the worker. Thus the compositor cannot start without an
   armed crash-recovery path.
5. On ordinary logout, the worker terminates its managed process group, closes
   PAM, and exits. The supervisor reaps it, stops the retained logind scope,
   confirms that the scope is inactive, then sends an explicit normal-complete
   message to the guardian and reaps the guardian.
6. If the supervisor disappears without sending normal completion, the
   guardian observes EOF/hangup. It waits on the worker pidfd for a bounded
   PAM-cleanup grace period. Once the worker exits, it stops the validated
   logind scope and waits for `ActiveState=inactive`.
7. If the worker does not exit within the grace period, the guardian stops the
   scope as emergency containment. This can prevent a leaked session cgroup,
   but cannot promise that a wedged worker completed PAM close first. The
   failure must be logged and surfaced through system logs.

The guardian's stop operation must be idempotent: an already inactive scope
is success. A missing unit, failed state, system-bus failure, or timeout is a
reported cleanup failure, not a silent success.

## Failure handling and race rules

- Guardian startup, pidfd creation/transfer, arm acknowledgement, or protocol
  validation failure is fail-closed: do not approve the worker; close its
  control channel and let its pre-compositor PAM cleanup path run.
- The guardian must not stop the scope while the worker is still expected to
  close PAM, except after the explicit bounded emergency deadline.
- The normal-completion message is sent only after the supervisor has confirmed
  the scope inactive. If the supervisor crashes between scope stop and that
  message, the guardian repeats the idempotent inactive check.
- The guardian must not depend on the supervisor to reap it after a crash.
  Normal operation does reap it; after parent death it is reparented and exits
  after its own cleanup.
- The guardian must use a bounded systemd client invocation, no shell, a
  minimal environment, and the same strict scope-name validation as the
  supervisor.
- The guardian must close all unrelated file descriptors and must not share
  the worker's control socket. The only transferred process handle is the
  worker pidfd.

## Limits and validation

A separate guardian process protects against abrupt supervisor death (for
example SIGKILL) while the guardian itself remains alive. It does not protect
against system shutdown, guardian termination, kernel failure, or a system
manager operation that kills the guardian's own service/cgroup. A later
hardening step may place the guardian in a dedicated systemd-managed unit
outside both the greeter and user-session cgroups.

Before enabling this in production, add unit tests for pidfd transfer,
protocol framing, scope validation, supervisor EOF, normal completion,
worker-exit grace, and idempotent scope cleanup. Extend the disposable Debian
13 VM harness to kill the supervisor after approval and verify that the worker
closes PAM and that the guardian removes both the scope and a descendant that
escaped its process group. Also test the emergency deadline with a deliberately
stalled worker. The existing workspace CI and mocked unit tests cannot replace
these real logind/cgroup tests.
