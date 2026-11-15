# PAM service profiles

BubbleWM's login helpers use the fixed PAM service name `bubblewm`. The
`bubblewm.debian` profile is a Debian-compatible starting point; a distribution
package should install its reviewed profile as `/etc/pam.d/bubblewm`.

The profile reuses the host's `login` stack for password authentication,
account policy and session setup, rejects root for graphical login, and enables
GNOME Keyring/KWallet hooks when those modules are installed. Other
distributions must package an equivalent service that includes their normal
authentication, account, session and user-manager modules.

This repository does not install the profile yet. `run_login_session` cannot
open a login until the target system has a `bubblewm` PAM service configured.
