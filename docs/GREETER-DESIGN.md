# BubbleGreeter design

## Goal

BubbleGreeter is the graphical login interface for BubbleSession. It must be usable before a user session exists and must return after logout or a failed session startup. It is not BubbleWM's in-session lock screen.

This document defines the process and credential boundaries before the graphical frontend is implemented. It does not claim that greeter, seat/VT handoff, or a production display-manager service already works.

## Process model

The intended system has three distinct responsibilities:

1. **System supervisor (root):** owns the active VT/seat transition, starts and stops the greeter and login helper, and supervises the complete session lifecycle. It does not render the login UI or collect the password itself.
2. **Greeter (dedicated unprivileged account):** renders the login UI, selects a local account, collects the password, and submits one login attempt. It must not run as root and must not receive PAM handles or systemd authority.
3. **PAM worker (short-lived root helper):** receives the password through an anonymous pipe, opens the PAM/logind session, and starts the fixed /usr/bin/bubblewm compositor as the authenticated user. The existing bubblewm-session supervisor validates the worker's session metadata and performs logind scope cleanup.

The greeter must never launch a shell or construct an executable path from user-controlled input. The selected account name is validated as an account identifier and passed as an argument; the password is sent only through a private pipe on standard input. It must not appear in argv, environment variables, logs, crash reports, or BubbleWM's ordinary desktop IPC.

## Display and seat ownership

The greeter needs a graphical surface before the user's compositor exists. It therefore cannot simply be an ordinary client of the user's BubbleWM instance. A system greeter compositor/session must own the display while the login screen is visible. The greeter UI runs unprivileged inside that environment.

On successful authentication, the system supervisor coordinates stopping the greeter environment and handing the seat/VT to the authenticated BubbleWM session. On logout, compositor crash, or startup failure, it must terminate the complete user session scope, release the seat, and restore the greeter. The implementation must use the supported logind/libseat session mechanism; it must not independently steal DRM devices or race another seat manager.

The exact greeter compositor/backend and handoff mechanism must be proven with real logind/libseat and VT integration tests before being enabled by a system package. The current DRM backend's pause/resume support alone is not a complete greeter-to-user-session handoff.

## Login-attempt lifecycle

1. The supervisor starts the greeter on the managed seat.
2. The greeter obtains the list of eligible local accounts from a constrained system interface; it does not parse privileged account files itself.
3. The user selects an account and enters a password.
4. The greeter sends the account identifier and password to the supervisor using a private, per-attempt channel. The channel must be local, bounded, authenticated by peer credentials, and closed after the attempt.
5. The supervisor creates an anonymous pipe and starts the fixed bubblewm-session run USER /dev/ttyN entry point with the password pipe as stdin. The supervisor must not read or log the password.
6. The existing worker validates PAM credentials and account policy, opens the PAM session, and reports bounded session metadata over its private control socket. The supervisor verifies that metadata against logind before approving compositor startup.
7. BubbleWM runs with the authenticated user's UID/GID, groups, runtime directory, user manager and filtered session environment.
8. When BubbleWM exits, the worker closes PAM and the supervisor stops and verifies the logind scope. Only then may the greeter be restored.

Failed authentication returns a generic error to the greeter, applies a bounded retry delay/rate limit, and allows another attempt. Internal PAM details are logged only in a way that does not disclose the password or unnecessarily reveal account state.

## Security requirements

- No setuid installation of bubblewm-session; only the system supervisor may invoke it with the required privilege.
- No password in command-line arguments, environment variables, files, persistent IPC, or logs.
- Password buffers are bounded, short-lived, and explicitly zeroed after use.
- Per-attempt channels are private, permission-restricted, and checked using peer credentials; reject unexpected descriptors and malformed frames.
- The greeter account has no access to user session credentials or privileged control sockets.
- The greeter cannot choose an executable, config path, environment override, arbitrary VT, or arbitrary command.
- User compositor and desktop applications never run as root.
- Greeter and user-session process trees are separately supervised and fully reaped on every success and failure path.
- No automatic PAM profile overwrite. Each distribution must ship and review its own bubblewm PAM service policy.

## Implementation gates

1. **Contract and threat model:** this document; no production login is enabled.
2. **Greeter frontend:** build and test the unprivileged UI independently, with a mock login transport and no real password submission.
3. **Private login transport:** implement bounded request/reply framing, peer-credential checks, descriptor hygiene, cancellation and tests.
4. **System supervisor:** implement greeter startup/restart and controlled invocation of the existing session helper.
5. **Seat/VT handoff:** validate greeter-to-session and return transitions against real logind/libseat in virtual machines.
6. **End-to-end hardening:** test wrong passwords, account policy failures, PAM failures, compositor crashes, forced termination, escaped descendants, supervisor restart and shutdown/reboot before packaging or enabling it.

No later gate may be marked complete based only on unit tests for an earlier gate. Until the end-to-end gates pass, BubbleGreeter must be described as development-only and BubbleWM must not replace a working display manager.
