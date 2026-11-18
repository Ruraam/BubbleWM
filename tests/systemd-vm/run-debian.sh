#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this test as root inside a disposable Debian 13 VM." >&2
    exit 1
fi

for command in apt-get cc loginctl systemctl useradd userdel chpasswd openssl; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "Missing command: $command" >&2
        exit 1
    }
done

if ! systemctl is-active --quiet systemd-logind.service; then
    echo "systemd-logind.service must be active; boot a normal Debian 13 VM." >&2
    exit 1
fi

if [ ! -f /etc/pam.d/common-session ] ||
   ! grep -Eq '^[[:space:]]*session[[:space:]].*pam_systemd\.so' /etc/pam.d/common-session; then
    echo "Debian common-session must include pam_systemd.so." >&2
    exit 1
fi

pam_systemd_count=$(grep -Ec '^[[:space:]]*session[[:space:]].*pam_systemd\.so' /etc/pam.d/common-session || true)
if [ "$pam_systemd_count" -ne 1 ]; then
    echo "Expected exactly one pam_systemd.so entry in common-session; found $pam_systemd_count." >&2
    exit 1
fi

apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y build-essential libpam0g-dev libsystemd-dev

if [ ! -e /etc/pam.d/bubblewm ]; then
    install -m 0644 "$(dirname "$0")/../../packaging/pam/bubblewm.debian" /etc/pam.d/bubblewm
fi

test_user="bwm-pam-test"
test_password=$(openssl rand -hex 24)
test_log=$(mktemp)
probe=$(mktemp)
cleanup() {
    if [ -n "${probe:-}" ]; then rm -f "$probe"; fi
    if [ -n "${test_log:-}" ]; then rm -f "$test_log"; fi
    userdel -r "$test_user" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

if id "$test_user" >/dev/null 2>&1; then
    echo "Refusing to reuse existing account $test_user." >&2
    exit 1
fi
useradd --create-home --shell /bin/bash "$test_user"
printf '%s:%s\n' "$test_user" "$test_password" | chpasswd

cc -std=c11 -O2 -Wall -Wextra -Werror \
    "$(dirname "$0")/pam-session-probe.c" -o "$probe" -lpam -lsystemd

# A real VT is required for a full seat/VT assertion. /dev/tty1 is supplied
# as PAM_TTY here; the VM procedure should be launched from its local console.
printf '%s\n' "$test_password" | BUBBLEWM_TEST_HOLD_SECONDS=8 \
    "$probe" "$test_user" /dev/tty1 >"$test_log" 2>&1 &
probe_pid=$!

attempt=0
while [ ! -s "$test_log" ] && kill -0 "$probe_pid" 2>/dev/null; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 100 ]; then
        echo "Timed out waiting for PAM session registration." >&2
        cat "$test_log" >&2
        exit 1
    fi
    sleep 0.1
done

if ! grep -q '^SESSION_ID=' "$test_log"; then
    wait "$probe_pid" || true
    cat "$test_log" >&2
    echo "PAM/logind session open failed." >&2
    exit 1
fi

session_id=$(sed -n 's/^SESSION_ID=\([^ ]*\).*/\1/p' "$test_log")
scope=$(sed -n 's/^SESSION_ID=[^ ]* SCOPE=\([^ ]*\).*/\1/p' "$test_log")
[ -n "$session_id" ] && [ -n "$scope ] || {
    echo "Probe returned malformed session metadata." >&2
    exit 1
}

loginctl show-session "$session_id" --property=Scope --property=State >"$test_log.state"
grep -Fx "Scope=$scope" "$test_log.state" >/dev/null
grep -Fx "State=active" "$test_log.state" >/dev/null
systemctl is-active --quiet "$scope"

wait "$probe_pid" || {
    cat "$test_log" >&2
    echo "PAM close/end failed." >&2
    exit 1
}

# pam_systemd should release the session scope when pam_close_session runs.
attempt=0
while systemctl is-active --quiet "$scope"; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 50 ]; then
        echo "Session scope remained active after pam_close_session: $scope" >&2
        exit 1
    fi
    sleep 0.1
done

rm -f "$test_log.state"
echo "PASS: real PAM authentication/open/close and logind scope lifecycle ($session_id)."
