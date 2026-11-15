# PAM service profiles

BubbleWM's login helpers use the fixed PAM service name `bubblewm`. The
`bubblewm.debian` profile is a Debian-compatible starting point; a distribution
package should install its reviewed profile as `/etc/pam.d/bubblewm`.

The profile reuses the host's `login` stack for password authentication,
account policy and session setup, rejects root for graphical login, and enables
GNOME Keyring/KWallet hooks when those modules are installed. Other
distributions must package an equivalent service that includes their normal
authentication, account, session and user-manager modules.

This repository does not install the profile yet. The `bubblewm-session`
helper is an early root-only entry point, not a complete display manager. It
cannot open a login until the target system has a reviewed `bubblewm` PAM
service configured. The helper expects the system package to install
`packaging/session.toml` as root-owned `/etc/bubblewm/session.toml`, and the
compositor at `/usr/bin/bubblewm`.
