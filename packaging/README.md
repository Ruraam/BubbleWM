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
- `/etc/bubblewm/session.toml` — root-managed system session configuration;
- `/etc/pam.d/bubblewm` — reviewed Debian-compatible PAM service profile.

The PAM file is staged as a package conffile candidate. A Debian package must
declare it as a conffile and must not overwrite an administrator's existing
`/etc/pam.d/bubblewm` during upgrades. The live-host `install-pam` target
refuses to replace an existing file; it is intended only for a deliberate,
manual installation after reviewing the host policy.

The Debian profile reuses `/etc/pam.d/login`. Before enabling graphical login,
verify that the host's included session stack invokes `pam_systemd.so` exactly
once. The profile must be reviewed for the target Debian release and local PAM
policy. Never blindly append another `pam_systemd.so` line.

The install target uses ordinary executable permissions. It does **not** make
`bubblewm-session` setuid and does not grant privilege by itself. A production
system supervisor must launch it with the required privileges and provide the
password through its anonymous stdin pipe.

This is a distribution-neutral staging primitive, not yet a complete package
or display manager. Greeter integration, VT/seat handoff, package maintainer
scripts remain separate work. A real-PAM/logind probe under
`tests/systemd-vm` is provided for a disposable Debian 13 VM with
systemd-logind active; it must still be executed on a real VM before claiming
runtime validation.
