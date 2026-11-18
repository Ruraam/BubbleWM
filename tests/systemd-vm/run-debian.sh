#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this test as root inside a disposable Debian 13 VM." >&2
    exit 1
fi

for command in apt-get cc loginctl systemctl useradd userdel chpasswd openssl grep sed; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "Missing command: $command" >&2
        exit 1
    }
done

if ! systemctl is-active --quiet systemd-logind.service; then
    echo "systemd-logind.service must be active; boot a normal Debian 13 VM." >&2
    exit 1
fi

if [ ! -f /etc/pam.d/login ] ||
   ! grep -Eq '^[[:space:]]*@include[[:space:]]+common-session|^[[:space:]]*session[[:space:]]+include[[:space:]]+common-session' /etc/pam.d/login; then
    echo "Debian login PAM stack must include common-session." >&2
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

test_user="bwm-pam-test"
test_password=$(openssl rand -hex 24)
test_log=$(mktemp)
probe=$(mktemp)
test_user_created=0
pam_service_created=0
probe_pid=
escaped_pid=
scope_to_stop=
cleanup() {
    if [ -z "$scope_to_stop" ] && [ -f "$test_log" ]; then
        scope_to_stop=$(sed -n 's/^SESSION_ID=[^ ]* SCOPE=\([^ ]*\).*/\1/p' "$test_log")
    fi
    if [ -z "$escaped_pid" ] && [ -f "$test_log" ]; then
        escaped_pid=$(sed -n 's/^ESCAPED_PID=\([0-9][0-9]*\)$/\1/p' "$test_log")
    fi
    if [ -n "$scope_to_stop" ]; then
        systemctl stop "$scope_to_stop" >/dev/null 2>&1 || true
    fi
    if [ -n "$probe_pid" ]; then
        if kill -0 "$probe_pid" 2>/dev/null; then
            kill -TERM "$probe_pid" 2>/dev/null || true
            attempt=0
            while kill -0 "$probe_pid" 2>/dev/null && [ "$attempt" -lt 20 ]; do
                attempt=$((attempt + 1))
                sleep 0.1
            done
            if kill -0 "$probe_pid" 2>/dev/null; then
                kill -KILL "$probe_pid" 2>/dev/null || true
            fi
        fi
        wait "$probe_pid" 2>/dev/null || true
        probe_pid=
    fi
    if [ "$test_user_created" -eq 1 ]; then
        userdel -r "$test_user" >/dev/null 2>&1 || true
    fi
    if [ "$pam_service_created" -eq 1 ]; then
        rm -f /etc/pam.d/bubblewm
    fi
    rm -f "$probe" "$test_log" "$test_log.state" "$test_log.failure"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

if id "$test_user" >/dev/null 2>&1; then
    echo "Refusing to reuse existing account $test_user." >&2
    exit 1
fi
if [ -e /etc/pam.d/bubblewm ]; then
    echo "/etc/pam.d/bubblewm already exists; refusing to test an unknown host policy." >&2
    exit 1
fi

install -m 0644 "$(dirname "$0")/../../packaging/pam/bubblewm.debian" /etc/pam.d/bubblewm
pam_service_created=1

useradd --create-home --shell /bin/bash "$test_user"
test_user_created=1
printf '%s:%s\n' "$test_user" "$test_password" | chpasswd

cc -std=c11 -O2 -Wall -Wextra -Werror \
    "$(dirname "$0")/pam-session-probe.c" -o "$probe" -lpam -lsystemd

# First prove that a bad password is rejected without opening a logind session.
wrong_password=$(openssl rand -hex 24)
if printf '%s\n' "$wrong_password" | "$probe" "$test_user" /dev/tty1 >"$test_log.failure" 2>&1; then
    cat "$test_log.failure" >&2
    echo "PAM unexpectedly accepted an invalid password." >&2
    exit 1
fi
if grep -q '^SESSION_ID=' "$test_log.failure"; then
    cat "$test_log.failure" >&2
    echo "A session was reported after failed authentication." >&2
    exit 1
fi

# A real VT is required for a full seat/VT assertion. /dev/tty1 is supplied
# as PAM_TTY here; launch from the VM's local console for meaningful VT data.
printf '%s\n' "$test_password" | BUBBLEWM_TEST_HOLD_SECONDS=8 BUBBLEWM_TEST_ESCAPE_GROUP=1 \
    "$probe" "$test_user" /dev/tty1 >"$test_log" 2>&1 &
probe_pid=$!

attempt=0
while ! grep -q '^ESCAPED_PID=' "$test_log" && kill -0 "$probe_pid" 2>/dev/null; do
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
escaped_pid=$(sed -n 's/^ESCAPED_PID=\([0-9][0-9]*\)$/\1/p' "$test_log")
scope_to_stop=$scope
[ -n "$session_id" ] && [ -n "$scope" ] && [ -n "$escaped_pid" ] || {
    echo "Probe returned malformed session metadata." >&2
    exit 1
}

loginctl show-session "$session_id" --property=Scope --property=State >"$test_log.state"
grep -Fx "Scope=$scope" "$test_log.state" >/dev/null
grep -Fx "State=active" "$test_log.state" >/dev/null
systemctl is-active --quiet "$scope"

if ! wait "$probe_pid"; then
    probe_pid=
    cat "$test_log" >&2
    echo "PAM close/end failed." >&2
    exit 1
fi
probe_pid=

# The test child has called setsid(): process-group signalling cannot reach it.
# The supervisor must stop the retained logind scope after the PAM worker exits.
if ! kill -0 "$escaped_pid" 2>/dev/null; then
    echo "Escaped-group test child exited before supervisor scope cleanup." >&2
    exit 1
fi
systemctl stop "$scope"
attempt=0
while systemctl is-active --quiet "$scope"; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 50 ]; then
        echo "Session scope remained active after supervisor cleanup: $scope" >&2
        exit 1
    fi
    sleep 0.1
done

# Accept a zombie as terminated: PID 1 may need a scheduling turn to reap it.
attempt=0
while [ -d "/proc/$escaped_pid" ]; do
    child_state=$(awk '{print $3}' "/proc/$escaped_pid/stat" 2>/dev/null || true)
    [ "$child_state" = Z ] && break
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 50 ]; then
        echo "Escaped process survived logind scope cleanup: $escaped_pid" >&2
        exit 1
    fi
    sleep 0.1
done
scope_to_stop=

echo "PASS: PAM authentication/open/close and supervisor cleanup of an escaped process-group descendant ($session_id)."
