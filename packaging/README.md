# System installation staging

The repository provides a staged installation target for package builders:

```sh
make -f packaging/Makefile
make -f packaging/Makefile install DESTDIR="$PWD/pkgroot"
```

The staging tree contains:

- `/usr/bin/bubblewm` — Wayland compositor;
- `/usr/bin/bubblewm-session` — privileged session supervisor entry point;
- `/usr/bin/bubblewm-config` — configuration utility;
- `/usr/libexec/bubblewm-session-worker` — dedicated PAM-owning worker;
- `/etc/bubblewm/session.toml` — root-managed system session configuration.

The install target uses ordinary executable permissions. It does **not** make
`bubblewm-session` setuid and does not grant privilege by itself. A production
system supervisor must launch it with the required privileges and provide the
password through its anonymous stdin pipe.

The PAM profile is deliberately not installed or enabled automatically. Review
`pam/README.md` and the target distribution's PAM policy before installing an
equivalent service as `/etc/pam.d/bubblewm`. In particular, verify that
`pam_systemd.so` runs exactly once. Do not overwrite a host PAM policy
automatically.

This is a staging primitive, not yet a complete Debian package or display
manager. Greeter integration, VT/seat handoff, package maintainer scripts and
real-systemd VM tests remain separate work.
