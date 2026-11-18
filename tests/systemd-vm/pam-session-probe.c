#define _GNU_SOURCE
#include <security/pam_appl.h>
#include <security/pam_misc.h>
#include <systemd/sd-login.h>

#include <errno.h>
#include <pwd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#define PASSWORD_MAX 1024

struct conversation_data {
    char password[PASSWORD_MAX + 1];
};

static int converse(int count, const struct pam_message **messages,
                    struct pam_response **responses, void *data) {
    struct conversation_data *conversation = data;
    struct pam_response *reply;

    if (count <= 0 || messages == NULL || responses == NULL || conversation == NULL)
        return PAM_CONV_ERR;

    reply = calloc((size_t)count, sizeof(*reply));
    if (reply == NULL)
        return PAM_BUF_ERR;

    for (int i = 0; i < count; ++i) {
        if (messages[i] == NULL)
            goto failure;
        switch (messages[i]->msg_style) {
        case PAM_PROMPT_ECHO_OFF:
            reply[i].resp = strdup(conversation->password);
            if (reply[i].resp == NULL)
                goto failure;
            break;
        case PAM_TEXT_INFO:
        case PAM_ERROR_MSG:
            reply[i].resp = NULL;
            break;
        case PAM_PROMPT_ECHO_ON:
        default:
            goto failure;
        }
    }
    *responses = reply;
    return PAM_SUCCESS;

failure:
    for (int i = 0; i < count; ++i)
        free(reply[i].resp);
    free(reply);
    return PAM_CONV_ERR;
}

static void wipe(void *memory, size_t length) {
    volatile unsigned char *bytes = memory;
    while (length-- > 0)
        *bytes++ = 0;
}

static int read_password(char *buffer, size_t capacity) {
    size_t used = 0;
    int ch;

    while ((ch = getchar()) != EOF && ch != '\n') {
        if (used + 1 >= capacity)
            return -1;
        buffer[used++] = (char)ch;
    }
    if (used > 0 && buffer[used - 1] == '\r')
        --used;
    if (used == 0)
        return -1;
    buffer[used] = '\0';
    return 0;
}

static int validate_runtime_dir(const char *path, uid_t expected_uid) {
    struct stat info;
    if (path == NULL || path[0] != '/' || lstat(path, &info) != 0)
        return -1;
    if (!S_ISDIR(info.st_mode) || info.st_uid != expected_uid ||
        (info.st_mode & 0077) != 0)
        return -1;
    return 0;
}

int main(int argc, char **argv) {
    struct conversation_data data = {{0}};
    struct pam_conv conversation = {converse, &data};
    pam_handle_t *pamh = NULL;
    struct passwd *account;
    char *session_id = NULL;
    char *pid_session = NULL;
    char *scope = NULL;
    const char *runtime_dir;
    const char *tty;
    uid_t session_uid = (uid_t)-1;
    int status = 1;
    int pam_status;
    const char *hold_text;
    unsigned int hold_seconds = 3;

    if (geteuid() != 0) {
        fprintf(stderr, "run this probe as root inside a disposable VM\n");
        return 2;
    }
    if (argc != 3) {
        fprintf(stderr, "usage: pam-session-probe USER /dev/ttyN\n");
        return 2;
    }
    account = getpwnam(argv[1]);
    if (account == NULL || account->pw_uid == 0) {
        fprintf(stderr, "test account must exist and must not be root\n");
        return 2;
    }
    tty = argv[2];
    if (strncmp(tty, "/dev/tty", 8) != 0 || tty[8] == '\0') {
        fprintf(stderr, "a virtual terminal path is required\n");
        return 2;
    }
    if (read_password(data.password, sizeof(data.password)) != 0) {
        fprintf(stderr, "could not read a non-empty password from stdin\n");
        wipe(&data, sizeof(data));
        return 2;
    }

    pam_status = pam_start("bubblewm", argv[1], &conversation, &pamh);
    if (pam_status != PAM_SUCCESS)
        goto cleanup;
    pam_status = pam_set_item(pamh, PAM_TTY, tty);
    if (pam_status != PAM_SUCCESS)
        goto cleanup;
    if (pam_putenv(pamh, "XDG_SESSION_TYPE=wayland") != PAM_SUCCESS ||
        pam_putenv(pamh, "XDG_CURRENT_DESKTOP=BubbleWM") != PAM_SUCCESS ||
        pam_putenv(pamh, "XDG_SESSION_DESKTOP=BubbleWM") != PAM_SUCCESS) {
        pam_status = PAM_SYSTEM_ERR;
        goto cleanup;
    }
    pam_status = pam_authenticate(pamh, 0);
    if (pam_status != PAM_SUCCESS)
        goto cleanup;
    pam_status = pam_acct_mgmt(pamh, 0);
    if (pam_status != PAM_SUCCESS)
        goto cleanup;
    pam_status = pam_open_session(pamh, 0);
    if (pam_status != PAM_SUCCESS)
        goto cleanup;

    session_id = (char *)pam_getenv(pamh, "XDG_SESSION_ID");
    runtime_dir = pam_getenv(pamh, "XDG_RUNTIME_DIR");
    if (session_id == NULL || session_id[0] == '\0' ||
        runtime_dir == NULL || validate_runtime_dir(runtime_dir, account->pw_uid) != 0) {
        fprintf(stderr, "PAM did not provide a valid logind session/runtime directory\n");
        goto close_session;
    }
    if (sd_pid_get_session(getpid(), &pid_session) < 0 ||
        strcmp(pid_session, session_id) != 0 ||
        sd_session_get_uid(session_id, &session_uid) < 0 ||
        session_uid != account->pw_uid) {
        fprintf(stderr, "logind could not independently resolve this PAM session\\n");
        goto close_session;
    }
    free(pid_session);
    pid_session = NULL;
    if (sd_pid_get_unit(getpid(), &scope) < 0 || scope == NULL ||
        strncmp(scope, "session-", 8) != 0) {
        fprintf(stderr, "process is not in a logind session scope\n");
        goto close_session;
    }

    printf("SESSION_ID=%s SCOPE=%s UID=%u\n", session_id, scope, (unsigned)session_uid);
    fflush(stdout);
    hold_text = getenv("BUBBLEWM_TEST_HOLD_SECONDS");
    if (hold_text != NULL) {
        char *end = NULL;
        unsigned long parsed = strtoul(hold_text, &end, 10);
        if (end != hold_text && *end == '\0' && parsed <= 30)
            hold_seconds = (unsigned int)parsed;
    }
    sleep(hold_seconds);
    status = 0;

close_session:
    pam_status = pam_close_session(pamh, 0);
    if (pam_status != PAM_SUCCESS) {
        fprintf(stderr, "pam_close_session failed: %s\n", pam_strerror(pamh, pam_status));
        status = 1;
    }
cleanup:
    if (pamh != NULL) {
        int end_status = pam_end(pamh, pam_status);
        if (end_status != PAM_SUCCESS) {
            fprintf(stderr, "pam_end failed: %s\n", pam_strerror(NULL, end_status));
            status = 1;
        }
    }
    wipe(&data, sizeof(data));
    /* session_id is owned by PAM. */
    free(pid_session);
    free(scope);
    return status;
}
