# BubbleWM staged installation.
#
# Build with:  make -f packaging/Makefile
# Stage with:  make -f packaging/Makefile install DESTDIR=/tmp/bubblewm-root
#
# The package builder must own the staged files as root:root when producing a
# system package. bubblewm-session is intentionally NOT setuid: it must only
# be started by a privileged system supervisor.

CARGO ?= cargo
PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin
LIBEXECDIR ?= $(PREFIX)/libexec
SYSCONFDIR ?= /etc
PAMDIR ?= $(SYSCONFDIR)/pam.d
DESTDIR ?=

.PHONY: all install install-pam clean

all:
	$(CARGO) build --release --bin bubblewm --bin bubblewm-session --bin bubblewm-session-worker --bin bubblewm-config

install: all
	install -D -m 0755 target/release/bubblewm "$(DESTDIR)$(BINDIR)/bubblewm"
	install -D -m 0755 target/release/bubblewm-session "$(DESTDIR)$(BINDIR)/bubblewm-session"
	install -D -m 0755 target/release/bubblewm-session-worker "$(DESTDIR)$(LIBEXECDIR)/bubblewm-session-worker"
	install -D -m 0755 target/release/bubblewm-config "$(DESTDIR)$(BINDIR)/bubblewm-config"
	install -D -m 0644 packaging/session.toml "$(DESTDIR)$(SYSCONFDIR)/bubblewm/session.toml"
	install -D -m 0644 packaging/pam/bubblewm.debian "$(DESTDIR)$(PAMDIR)/bubblewm"

# For a live host only: install the reviewed Debian PAM profile without
# replacing an administrator-managed service. Package builders should use
# DESTDIR with the normal install target instead.
install-pam:
	@test -n "$(DESTDIR)" || { \
		test ! -e "$(PAMDIR)/bubblewm" || { echo "Refusing to overwrite $(PAMDIR)/bubblewm" >&2; exit 1; }; \
	}
	install -D -m 0644 packaging/pam/bubblewm.debian "$(DESTDIR)$(PAMDIR)/bubblewm"

clean:
	$(CARGO) clean
