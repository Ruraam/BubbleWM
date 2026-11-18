# PAM service profiles

BubbleWM's login helpers use the fixed PAM service name `bubblewm`. The
`bubblewm.debian` profile is a Debian-compatible starting point and is staged
as `/etc/pam.d/bubblewm` by the packaging Makefile.

The profile reuses the host's `login` stack for password authentication,
account policy and session setup, rejects root for graphical login, and enables
GNOME Keyring/KWallet hooks when those modules are installed. On Debian, the
included `login` stack normally reaches `common-session`, where
`pam_systemd.so` is configured by the host's PAM policy. A package or system
administrator must verify that it runs exactly once; do not add a second
instance blindly.

Other distributions must package an equivalent service that includes their
normal authentication, account, session and user-manager modules. The profile
must be reviewed against the target distribution's PAM policy before use.

The staged file is a package conffile candidate. Package maintainer scripts
must preserve administrator changes and must never replace an existing service
silently. For manual installation on a live host, use
`make -f packaging/Makefile install-pam`; this target refuses to overwrite an
existing `/etc/pam.d/bubblewm`.

The systemd VM probe in `tests/systemd-vm` exercises actual PAM authentication,
account checks, session open/close, and logind scope registration. It requires
a disposable Debian 13 VM, an active systemd-logind instance, and root access.
