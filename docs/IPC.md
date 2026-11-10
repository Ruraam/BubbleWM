# BubbleWM IPC lifecycle

The Linux socket transport is implemented by `bubble-ipc::unix`. BubbleWM's
application-side bridge is exposed by `bubblewm::ipc`.

## Thread ownership

The IPC runtime owns one named worker thread. It accepts local connections,
authenticates peer credentials, validates the protocol envelope and sends
commands through a bounded channel with a capacity of 64 requests.

The compositor owns the receiving half. It calls
`CoreCommandReceiver::dispatch_pending` from its event loop; the supplied
handler therefore runs on the compositor thread and is the only place where
requests may inspect or mutate compositor state.

The IPC worker waits up to two seconds for a response. Queue saturation,
a stopped receiver, or a compositor that does not dispatch requests in time
returns a structured `unavailable` response rather than blocking indefinitely.

## Intended compositor integration

The lifecycle belongs to the compositor runtime, not to BubblePanel and not to
a detached process. The intended startup sequence is:

1. Load and validate configuration.
2. Initialize the Smithay backend and compositor state.
3. Start `IpcRuntime::start_default()` and retain the returned runtime owner
   for the complete compositor lifetime.
4. Poll and dispatch IPC commands from the compositor event loop using
   `CoreCommandReceiver::dispatch_pending`.
5. On normal shutdown, call `IpcRuntime::shutdown()` before destroying
   compositor state. Dropping the runtime also requests shutdown and joins
   its worker as a fallback.

The event-loop handler must remain quick and must not perform blocking I/O.
It must validate workspace indices and feature availability against the
authoritative compositor state before returning success.

## Current implementation boundary

The bridge, bounded channel, timeout behavior and worker lifecycle are
implemented and covered by tests. The current `bubblewm` executable still
only loads configuration and exits; it does not create a Smithay backend or
event loop. Therefore it intentionally does not open the IPC socket yet.
Starting the listener before a compositor event loop exists would produce an
endpoint that cannot truthfully manage workspaces.

The next compositor milestone must instantiate the runtime during actual
compositor startup and poll the receiver as part of its event loop.
