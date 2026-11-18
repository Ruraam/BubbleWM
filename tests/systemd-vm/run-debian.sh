#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this test as root inside a disposable Debian 13 VM." >&2
    exit 1
fi

for command in apt-get awk cc loginctl systemd-analyze systemctl useradd userdel chpasswd openssl grep sed; do
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
test_tmpdir=$(mktemp -d)
logind_config="$test_tmpdir/logind.conf"
systemd-analyze cat-config systemd/logind.conf >"$logind_config"
kill_user_processes=$(awk -F= '
    /^[[:space:]]*KillUserProcesses[[:space:]]*=/ {
        value = $2
        sub(/[[:space:]]*#.*/, "", value)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    }
    END { print value == "" ? "yes" : value }
' "$logind_config")
kill_only_users=$(awk -F= '
    /^[[:space:]]*KillOnlyUsers[[:space:]]*=/ {
        value = $2
        sub(/[[:space:]]*#.*/, "", value)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    }
    END { print value }
' "$logind_config")
kill_exclude_users=$(awk -F= '
    /^[[:space:]]*KillExcludeUsers[[:space:]]*=/ {
        value = $2
        sub(/[[:space:]]*#.*/, "", value)
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    }
    END { print value }
' "$logind_config")
case "$kill_user_processes" in
    yes|true|1|on) kill_user_processes=yes ;;
    no|false|0|off) kill_user_processes=no ;;
    *)
        echo "Unrecognized effective KillUserProcesses value: $kill_user_processes" >&2
        exit 1
        ;;
esac
if [ "${BUBBLEWM_EXPECT_KILL_USER_PROCESSES:-}" != "$kill_user_processes" ]; then
    echo "Set BUBBLEWM_EXPECT_KILL_USER_PROCESSES=$kill_user_processes to match the active logind policy." >&2
    exit 1
fi
if [ -n "$kill_only_users" ]; then
    echo "KillOnlyUsers must be empty for this global policy test (found: $kill_only_users)." >&2
    exit 1
fi
case " $kill_exclude_users " in
    *" $test_user "*)
        echo "$test_user is excluded by KillExcludeUsers; refusing an ambiguous policy test." >&2
        exit 1
        ;;
esac
echo "Testing effective KillUserProcesses=$kill_user_processes."
mkfifo "$test_tmpdir/password"
test_user_created=0
pam_service_created=0
probe_pid=
writer_pid=
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
    if [ -n "$writer_pid" ]; then
        kill -TERM "$writer_pid" 2>/dev/null || true
        wait "$writer_pid" 2>/dev/null || true
        writer_pid=
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
    rm -rf "$test_tmpdir"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

process_is_running() {
    pid=$1
    [ -r "/proc/$pid/stat" ] || return 1
    state=$(awk '{print $3}' "/proc/$pid/stat" 2>/dev/null || true)
    [ -n "$state" ] && [ "$state" != Z ]
}

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
BUBBLEWM_TEST_HOLD_SECONDS=8 BUBBLEWM_TEST_ESCAPE_GROUP=1 \
    "$probe" "$test_user" /dev/tty1 <"$test_tmpdir/password" >"$test_log" 2>&1 &
probe_pid=$!
printf '%s\n' "$test_password" >"$test_tmpdir/password" &
writer_pid=$!
if ! wait "$writer_pid"; then
    writer_pid=
    echo "Could not deliver the test password to the PAM probe." >&2
    exit 1
fi
writer_pid=

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
# logind may already have stopped the scope (yes), or the supervisor must do so (no).
if [ "$kill_user_processes" = yes ]; then
    attempt=0
    while process_is_running "$escaped_pid" || systemctl is-active --quiet "$scope"; do
        attempt=$((attempt + 1))
        if [ "$attempt" -gt 50 ]; then
            echo "logind did not clean up the escaped child and session scope with KillUserProcesses=yes." >&2
            exit 1
        fi
        sleep 0.1
    done
    echo "PASS: logind automatically removed the escaped child and scope (KillUserProcesses=yes)."
else
    if ! process_is_running "$escaped_pid"; then
        echo "Escaped-group test child exited before supervisor scope cleanup with KillUserProcesses=no." >&2
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
    while process_is_running "$escaped_pid"; do
        attempt=$((attempt + 1))
        if [ "$attempt" -gt 50 ]; then
            echo "Escaped process survived supervisor scope cleanup: $escaped_pid" >&2
            exit 1
        fi
        sleep 0.1
    done
    echo "PASS: supervisor scope cleanup removed the escaped child (KillUserProcesses=no)."
fi
scope_to_stop=

echo "PASS: PAM authentication/open/close and supervisor cleanup of an escaped process-group descendant ($session_id)."
