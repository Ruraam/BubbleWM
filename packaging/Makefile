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
DESTDIR ?=

.PHONY: all install clean

all:
	$(CARGO) build --release --bin bubblewm --bin bubblewm-session --bin bubblewm-session-worker --bin bubblewm-config

install: all
	install -D -m 0755 target/release/bubblewm "$(DESTDIR)$(BINDIR)/bubblewm"
	install -D -m 0755 target/release/bubblewm-session "$(DESTDIR)$(BINDIR)/bubblewm-session"
	install -D -m 0755 target/release/bubblewm-session-worker "$(DESTDIR)$(LIBEXECDIR)/bubblewm-session-worker"
	install -D -m 0755 target/release/bubblewm-config "$(DESTDIR)$(BINDIR)/bubblewm-config"
	install -D -m 0644 packaging/session.toml "$(DESTDIR)$(SYSCONFDIR)/bubblewm/session.toml"

clean:
	$(CARGO) clean
