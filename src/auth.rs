//! PAM authentication primitives for BubbleWM's lock and login flows.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    io::Read,
    os::unix::ffi::OsStringExt,
    os::unix::fs::MetadataExt,
    os::unix::net::UnixStream,
    os::unix::process::CommandExt,
    os::fd::AsRawFd,
    path::PathBuf,
    process::{Child, Command, ExitStatus},
    ptr,
    sync::atomic::{Ordering, compiler_fence},
    thread,
    time::{Duration, Instant},
};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_CONV_ERR: c_int = 19;
const LOGIN_PAM_SERVICE: &str = "bubblewm";
const PAM_ESTABLISH_CRED: c_int = 0x0002;
const PAM_DELETE_CRED: c_int = 0x0004;
const PAM_TTY: c_int = 3;
const PAM_XDG_SESSION_TYPE: &CStr = c"XDG_SESSION_TYPE=wayland";
const PAM_XDG_CURRENT_DESKTOP: &CStr = c"XDG_CURRENT_DESKTOP=BubbleWM";
const PAM_XDG_SESSION_DESKTOP: &CStr = c"XDG_SESSION_DESKTOP=BubbleWM";

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConversation {
    conv: Option<
        unsafe extern "C" fn(
            num_msg: c_int,
            msg: *mut *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service_name: *const c_char,
        user: *const c_char,
        pam_conv: *const PamConversation,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_end(pamh: *mut c_void, pam_status: c_int) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_set_item(pamh: *mut c_void, item_type: c_int, item: *const c_void) -> c_int;
    fn pam_putenv(pamh: *mut c_void, name_value: *const c_char) -> c_int;
    fn pam_setcred(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_open_session(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_close_session(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_getenvlist(pamh: *mut c_void) -> *mut *mut c_char;
}

unsafe extern "C" fn conversation(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if count <= 0 || messages.is_null() || responses.is_null() || appdata.is_null() {
        return PAM_CONV_ERR;
    }
    let password = unsafe { CStr::from_ptr(appdata as *const c_char) };
    let response_size = std::mem::size_of::<PamResponse>();
    let response = unsafe { libc::calloc(count as usize, response_size) as *mut PamResponse };
    if response.is_null() {
        return PAM_CONV_ERR;
    }
    for index in 0..count as usize {
        let message = unsafe { *messages.add(index) };
        if message.is_null() {
            unsafe { free_responses(response, index) };
            unsafe { libc::free(response as *mut c_void) };
            return PAM_CONV_ERR;
        }
        let style = unsafe { (*message).msg_style };
        let destination = unsafe { response.add(index) };
        match style {
            PAM_PROMPT_ECHO_OFF => {
                if password.to_bytes().is_empty() {
                    unsafe { free_responses(response, index) };
                    unsafe { libc::free(response as *mut c_void) };
                    return PAM_CONV_ERR;
                }
                let copied = unsafe { libc::strdup(password.as_ptr()) };
                if copied.is_null() {
                    unsafe { free_responses(response, index) };
                    unsafe { libc::free(response as *mut c_void) };
                    return PAM_CONV_ERR;
                }
                unsafe { (*destination).resp = copied };
                unsafe { (*destination).resp_retcode = 0 };
            }
            PAM_PROMPT_ECHO_ON => {
                unsafe { free_responses(response, index) };
                unsafe { libc::free(response as *mut c_void) };
                return PAM_CONV_ERR;
            }
            PAM_ERROR_MSG | PAM_TEXT_INFO => {
                unsafe { (*destination).resp = ptr::null_mut() };
                unsafe { (*destination).resp_retcode = 0 };
            }
            _ => {
                unsafe { free_responses(response, index) };
                unsafe { libc::free(response as *mut c_void) };
                return PAM_CONV_ERR;
            }
        }
    }
    unsafe { *responses = response };
    PAM_SUCCESS
}

unsafe fn free_responses(responses: *mut PamResponse, count: usize) {
    for index in 0..count {
        let response = unsafe { (*responses.add(index)).resp };
        if !response.is_null() {
            unsafe { libc::free(response as *mut c_void) };
        }
    }
}

/// Current numeric user id of the compositor process.
pub fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Returns the current user's login name.
pub fn current_user() -> Option<String> {
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() || (*entry).pw_name.is_null() {
            return None;
        }
        Some(
            CStr::from_ptr((*entry).pw_name)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Authenticates the current user against a PAM service.
pub fn authenticate(password: &str, service: &str) -> Result<(), String> {
    let user = current_user().ok_or_else(|| "Utilisateur inconnu".to_owned())?;
    authenticate_user(&user, password, service)
}

/// Authenticates a selected login account using BubbleWM's fixed PAM service.
pub fn authenticate_login_user(user: &str, password: &str) -> Result<(), String> {
    authenticate_user(user, password, LOGIN_PAM_SERVICE)
}

/// Authenticates a selected local account against a PAM service.
///
/// This checks authentication and account policy only. A login manager must
/// still open and close the PAM session and establish the user's process
/// credentials before it starts the desktop.
pub fn authenticate_user(user: &str, password: &str, service: &str) -> Result<(), String> {
    let mut pam = PamHandle::start(user, password, service, None)?;
    pam.authenticate_account()
}

/// Account information needed to start the authenticated user's desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginIdentity {
    pub username: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
}

/// Owns a successfully opened PAM login session.
///
/// This function changes supplementary groups for the lifetime of the object.
/// Call it only from a dedicated, single-threaded root supervisor process; do
/// not call it from the compositor or greeter event loop, and do not create
/// threads while the object is alive. Keep it alive until the user session
/// child has exited, then call `close` (or drop it) so PAM session cleanup runs.
pub struct PamLoginSession {
    pam: PamHandle,
    groups: SupplementaryGroups,
    pub identity: LoginIdentity,
    pub environment: Vec<(std::ffi::OsString, std::ffi::OsString)>,
}

impl PamLoginSession {
    /// Authenticates a selected account, opens its PAM session, and keeps that
    /// session active until this object is closed or dropped.
    pub fn open(user: &str, password: &str, tty: &str) -> Result<Self, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("L'ouverture d'une session PAM exige les privilèges système".into());
        }
        if !process_is_single_threaded()? {
            return Err(
                "La session PAM doit être ouverte par un superviseur isolé sans threads".into(),
            );
        }
        validate_credentials(user, password, LOGIN_PAM_SERVICE)?;
        if !tty.starts_with("/dev/") || tty.len() > 256 || tty.chars().any(char::is_control) {
            return Err("Terminal PAM invalide".into());
        }

        let identity = lookup_login_identity(user)?;
        if identity.uid == 0 {
            return Err("La session graphique root est désactivée".into());
        }
        let user_c = CString::new(user).map_err(|_| "Identifiant invalide".to_owned())?;
        let mut groups = SupplementaryGroups::current()?;
        groups.switch_to(&user_c, identity.gid)?;

        let mut pam = PamHandle::start(user, password, LOGIN_PAM_SERVICE, Some(tty))?;
        pam.authenticate_account()?;
        pam.establish_credentials()?;
        pam.open_session()?;
        let environment = pam.environment();

        Ok(Self {
            pam,
            groups,
            identity,
            environment,
        })
    }

    /// Starts a trusted absolute-path program as the authenticated user with a
    /// clean, Wayland-only environment. The program and arguments must come
    /// from the privileged supervisor, not directly from greeter input. Keep
    /// this handle alive until the child exits, then close the handle.
    pub fn spawn_user_command(
        &self,
        program: &std::ffi::OsStr,
        arguments: &[std::ffi::OsString],
    ) -> Result<ManagedUserProcess, String> {
        if !self.groups.changed {
            return Err("La session PAM est déjà fermée".into());
        }
        if !std::path::Path::new(program).is_absolute() {
            return Err("Le programme de session doit être un chemin absolu".into());
        }
        let runtime_dir = self
            .environment_value("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", self.identity.uid)));
        validate_runtime_directory(&runtime_dir, self.identity.uid)?;

        let mut command = Command::new(program);
        command
            .args(arguments)
            .env_clear()
            .env("HOME", &self.identity.home)
            .env("USER", &self.identity.username)
            .env("LOGNAME", &self.identity.username)
            .env("SHELL", &self.identity.shell)
            .env(
                "PATH",
                "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin",
            )
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_SESSION_TYPE", "wayland")
            .env("XDG_CURRENT_DESKTOP", "BubbleWM")
            .env("XDG_SESSION_DESKTOP", "BubbleWM");
        for (name, value) in &self.environment {
            if pass_pam_environment(name) {
                command.env(name, value);
            }
        }
        let child = command
            .current_dir(&self.identity.home)
            .gid(self.identity.gid)
            .uid(self.identity.uid)
            .process_group(0)
            .spawn()
            .map_err(|error| format!("Impossible de lancer le processus de session: {error}"))?;
        Ok(ManagedUserProcess::new(child))
    }

    /// Builds the non-secret session metadata announced after PAM opens the session.
    ///
    /// The supervisor must compare this claim with an independent logind lookup.
    pub fn session_ready(&self) -> Result<crate::session_control::SessionReady, String> {
        let session_id = self
            .environment_value("XDG_SESSION_ID")
            .and_then(std::ffi::OsStr::to_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "PAM n'a fourni aucun identifiant de session logind".to_owned())?
            .to_owned();
        let runtime_dir = self
            .environment_value("XDG_RUNTIME_DIR")
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("/run/user/{}", self.identity.uid));
        let seat = self
            .environment_value("XDG_SEAT")
            .and_then(std::ffi::OsStr::to_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let vt_number = self
            .environment_value("XDG_VTNR")
            .and_then(std::ffi::OsStr::to_str)
            .map(|value| value.parse::<u32>())
            .transpose()
            .map_err(|_| "Numéro de terminal logind invalide".to_owned())?;

        Ok(crate::session_control::SessionReady {
            uid: self.identity.uid,
            session_id,
            runtime_dir,
            seat,
            vt_number,
        })
    }

    fn environment_value(&self, name: &str) -> Option<&std::ffi::OsStr> {
        self.environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_os_str())
    }

    /// Closes the PAM session and restores the supervisor's original groups.
    pub fn close(mut self) -> Result<(), String> {
        let pam_result = self.pam.finish();
        let groups_result = self.groups.restore();
        combine_cleanup_results(pam_result, groups_result)
    }
}

impl Drop for PamLoginSession {
    fn drop(&mut self) {
        if let Err(error) = self.pam.finish() {
            tracing::error!(%error, "PAM session cleanup failed");
        }
        if let Err(error) = self.groups.restore() {
            tracing::error!(%error, "supervisor group restoration failed");
        }
    }
}

/// Owns the session leader and its dedicated process group.
///
/// Call `terminate` when the user logs out or the compositor fails. Dropping
/// the handle force-stops any remaining processes in the group and reaps the
/// direct child, so an early return cannot leave ordinary session children
/// running under the user's account.
pub struct ManagedUserProcess {
    child: Child,
    process_group: libc::pid_t,
    child_status: Option<ExitStatus>,
}

impl ManagedUserProcess {
    fn new(child: Child) -> Self {
        let process_group = child.id() as libc::pid_t;
        Self {
            child,
            process_group,
            child_status: None,
        }
    }

    /// PID of the session leader.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Returns the leader's exit status when it has exited.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if self.child_status.is_some() {
            return Ok(self.child_status);
        }
        self.child_status = self.child.try_wait()?;
        Ok(self.child_status)
    }

    /// Waits for and reaps the session leader.
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(status) = self.child_status {
            return Ok(status);
        }
        let status = self.child.wait()?;
        self.child_status = Some(status);
        Ok(status)
    }

    /// Waits for the session leader while monitoring the private supervisor
    /// socket. A peer hangup or any unexpected post-approval byte is treated as
    /// supervisor loss and terminates the entire managed process group.
    pub fn wait_while_supervised(
        &mut self,
        channel: &mut UnixStream,
        poll_interval: Duration,
        termination_grace: Duration,
    ) -> std::io::Result<(ExitStatus, bool)> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok((status, false));
            }

            let mut descriptor = libc::pollfd {
                fd: channel.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            };
            let timeout = poll_interval.as_millis().min(i32::MAX as u128) as i32;
            let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                continue;
            }
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid supervisor control descriptor",
                ));
            }
            if descriptor.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                return self
                    .terminate(termination_grace)
                    .map(|status| (status, true));
            }
            if descriptor.revents & libc::POLLIN != 0 {
                let mut unexpected = [0_u8; 1];
                match channel.read(&mut unexpected) {
                    Ok(0) | Ok(_) => {
                        return self
                            .terminate(termination_grace)
                            .map(|status| (status, true));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => return Err(error),
                }
            }
        }
    }

    /// Sends SIGTERM to the session process group, then SIGKILL after the
    /// grace period if any member remains. The direct child is always reaped.
    pub fn terminate(&mut self, grace_period: Duration) -> std::io::Result<ExitStatus> {
        signal_process_group(self.process_group, libc::SIGTERM)?;
        let deadline = Instant::now() + grace_period;
        loop {
            let status = self.try_wait()?;
            if !process_group_exists(self.process_group)? {
                return match status {
                    Some(status) => Ok(status),
                    None => self.wait(),
                };
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            thread::sleep((deadline - now).min(Duration::from_millis(25)));
        }
        signal_process_group(self.process_group, libc::SIGKILL)?;
        self.wait()
    }
}

impl Drop for ManagedUserProcess {
    fn drop(&mut self) {
        if let Err(error) = signal_process_group(self.process_group, libc::SIGKILL) {
            tracing::error!(%error, "failed to stop user session process group");
        }
        if self.child_status.is_none()
            && let Err(error) = self.child.wait()
        {
            tracing::error!(%error, "failed to reap user session leader");
        }
    }
}

/// Runs one authenticated Wayland session from an isolated single-threaded
/// root helper. This composes PAM setup, user-command execution, process-group
/// cleanup and PAM teardown in the required order.
pub fn run_login_session(
    user: &str,
    password: &str,
    tty: &str,
    program: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
    termination_grace: Duration,
) -> Result<ExitStatus, String> {
    let session = PamLoginSession::open(user, password, tty)?;
    let mut process = session.spawn_user_command(program, arguments)?;
    let status = process
        .wait()
        .map_err(|error| format!("Impossible d'attendre la fin de la session: {error}"))?;
    let process_cleanup = process
        .terminate(termination_grace)
        .map_err(|error| format!("Impossible de terminer le groupe de session: {error}"));
    drop(process);
    let pam_cleanup = session.close();
    process_cleanup?;
    pam_cleanup?;
    Ok(status)
}

fn signal_process_group(process_group: libc::pid_t, signal: libc::c_int) -> std::io::Result<()> {
    if unsafe { libc::kill(-process_group, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn process_group_exists(process_group: libc::pid_t) -> std::io::Result<bool> {
    if unsafe { libc::kill(-process_group, 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

struct PamHandle {
    handle: *mut c_void,
    _user: CString,
    _service: CString,
    _password: CString,
    last_status: c_int,
    credentials_established: bool,
    session_open: bool,
}

impl PamHandle {
    fn start(user: &str, password: &str, service: &str, tty: Option<&str>) -> Result<Self, String> {
        validate_credentials(user, password, service)?;
        let user_c = CString::new(user).map_err(|_| "Identifiant invalide".to_owned())?;
        let service_c = CString::new(service).map_err(|_| "Service PAM invalide".to_owned())?;
        let mut password_c =
            CString::new(password).map_err(|_| "Mot de passe invalide".to_owned())?;
        let conversation = PamConversation {
            conv: Some(conversation),
            appdata_ptr: password_c.as_ptr() as *mut c_void,
        };
        let mut handle: *mut c_void = ptr::null_mut();
        let status = unsafe {
            pam_start(
                service_c.as_ptr(),
                user_c.as_ptr(),
                &conversation,
                &mut handle,
            )
        };
        if status != PAM_SUCCESS || handle.is_null() {
            if !handle.is_null() {
                unsafe { pam_end(handle, status) };
            }
            erase_cstring(&mut password_c);
            return Err(format!("PAM initialisation ({status})"));
        }

        let mut pam = Self {
            handle,
            _user: user_c,
            _service: service_c,
            _password: password_c,
            last_status: PAM_SUCCESS,
            credentials_established: false,
            session_open: false,
        };
        if let Some(tty) = tty {
            let tty_c = CString::new(tty).map_err(|_| "Terminal PAM invalide".to_owned())?;
            let status =
                unsafe { pam_set_item(pam.handle, PAM_TTY, tty_c.as_ptr() as *const c_void) };
            pam.last_status = status;
            if status != PAM_SUCCESS {
                return Err(format!("Impossible de définir le terminal PAM ({status})"));
            }
            for item in [
                PAM_XDG_SESSION_TYPE,
                PAM_XDG_CURRENT_DESKTOP,
                PAM_XDG_SESSION_DESKTOP,
            ] {
                let status = unsafe { pam_putenv(pam.handle, item.as_ptr()) };
                pam.last_status = status;
                if status != PAM_SUCCESS {
                    return Err(format!(
                        "Impossible de définir l'environnement PAM ({status})"
                    ));
                }
            }
        }
        Ok(pam)
    }

    fn authenticate_account(&mut self) -> Result<(), String> {
        let authenticated = unsafe { pam_authenticate(self.handle, 0) };
        self.last_status = authenticated;
        if authenticated != PAM_SUCCESS {
            self.erase_password();
            return Err("Mot de passe incorrect".into());
        }
        let account = unsafe { pam_acct_mgmt(self.handle, 0) };
        self.last_status = account;
        self.erase_password();
        if account != PAM_SUCCESS {
            return Err("Compte verrouillé ou expiré".into());
        }
        Ok(())
    }

    fn establish_credentials(&mut self) -> Result<(), String> {
        let status = unsafe { pam_setcred(self.handle, PAM_ESTABLISH_CRED) };
        self.last_status = status;
        if status != PAM_SUCCESS {
            return Err(format!(
                "Impossible d'établir les identifiants PAM ({status})"
            ));
        }
        self.credentials_established = true;
        Ok(())
    }

    fn open_session(&mut self) -> Result<(), String> {
        let status = unsafe { pam_open_session(self.handle, 0) };
        self.last_status = status;
        if status != PAM_SUCCESS {
            return Err(format!("Impossible d'ouvrir la session PAM ({status})"));
        }
        self.session_open = true;
        Ok(())
    }

    fn environment(&self) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        let entries = unsafe { pam_getenvlist(self.handle) };
        if entries.is_null() {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut index = 0;
        loop {
            let entry = unsafe { *entries.add(index) };
            if entry.is_null() {
                break;
            }
            let bytes = unsafe { CStr::from_ptr(entry) }.to_bytes();
            if let Some(separator) = bytes.iter().position(|byte| *byte == b'=')
                && separator > 0
            {
                result.push((
                    std::ffi::OsString::from_vec(bytes[..separator].to_vec()),
                    std::ffi::OsString::from_vec(bytes[separator + 1..].to_vec()),
                ));
            }
            unsafe { libc::free(entry as *mut c_void) };
            index += 1;
        }
        unsafe { libc::free(entries as *mut c_void) };
        result
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.handle.is_null() {
            return Ok(());
        }
        let mut errors = Vec::new();
        if self.session_open {
            let status = unsafe { pam_close_session(self.handle, 0) };
            self.last_status = status;
            self.session_open = false;
            if status != PAM_SUCCESS {
                errors.push(format!("Échec de fermeture de la session PAM ({status})"));
            }
        }
        if self.credentials_established {
            let status = unsafe { pam_setcred(self.handle, PAM_DELETE_CRED) };
            self.last_status = status;
            self.credentials_established = false;
            if status != PAM_SUCCESS {
                errors.push(format!(
                    "Échec de suppression des identifiants PAM ({status})"
                ));
            }
        }
        let status = unsafe { pam_end(self.handle, self.last_status) };
        self.handle = ptr::null_mut();
        if status != PAM_SUCCESS {
            errors.push(format!("Échec de libération du contexte PAM ({status})"));
        }
        self.erase_password();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn erase_password(&mut self) {
        erase_cstring(&mut self._password);
    }
}

fn erase_cstring(value: &mut CString) {
    let password = value.as_ptr() as *mut u8;
    let length = value.as_bytes_with_nul().len();
    for index in 0..length {
        unsafe { password.add(index).write_volatile(0) };
    }
    compiler_fence(Ordering::SeqCst);
}

impl Drop for PamHandle {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

struct SupplementaryGroups {
    original: Vec<libc::gid_t>,
    changed: bool,
}

impl SupplementaryGroups {
    fn current() -> Result<Self, String> {
        let count = unsafe { libc::getgroups(0, ptr::null_mut()) };
        if count < 0 {
            return Err(format!(
                "Impossible de lire les groupes du superviseur: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut original = vec![0; count as usize];
        let actual = unsafe { libc::getgroups(count, original.as_mut_ptr()) };
        if actual < 0 {
            return Err(format!(
                "Impossible de lire les groupes du superviseur: {}",
                std::io::Error::last_os_error()
            ));
        }
        original.truncate(actual as usize);
        Ok(Self {
            original,
            changed: false,
        })
    }

    fn switch_to(&mut self, user: &CString, primary_gid: u32) -> Result<(), String> {
        self.changed = true;
        if unsafe { libc::initgroups(user.as_ptr(), primary_gid as libc::gid_t) } != 0 {
            let error = std::io::Error::last_os_error();
            return match self.restore() {
                Ok(()) => Err(format!(
                    "Impossible de préparer les groupes du compte: {error}"
                )),
                Err(restore_error) => Err(format!(
                    "Impossible de préparer les groupes du compte: {error}; {restore_error}"
                )),
            };
        }
        Ok(())
    }

    fn restore(&mut self) -> Result<(), String> {
        if !self.changed {
            return Ok(());
        }
        let groups = if self.original.is_empty() {
            ptr::null()
        } else {
            self.original.as_ptr()
        };
        let status = unsafe { libc::setgroups(self.original.len(), groups) };
        if status != 0 {
            return Err(format!(
                "Impossible de restaurer les groupes du superviseur: {}",
                std::io::Error::last_os_error()
            ));
        }
        self.changed = false;
        Ok(())
    }
}

impl Drop for SupplementaryGroups {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            tracing::error!(%error, "supplementary group restoration failed");
        }
    }
}

fn validate_credentials(user: &str, password: &str, service: &str) -> Result<(), String> {
    if user.is_empty() || user.len() > 256 || user.chars().any(char::is_control) {
        return Err("Identifiant invalide".into());
    }
    if password.is_empty() {
        return Err("Mot de passe vide".into());
    }
    if password.len() > 1024 {
        return Err("Mot de passe trop long".into());
    }
    if service.is_empty() || service.len() > 128 {
        return Err("Service PAM invalide".into());
    }
    Ok(())
}

fn lookup_login_identity(user: &str) -> Result<LoginIdentity, String> {
    let user_c = CString::new(user).map_err(|_| "Identifiant invalide".to_owned())?;
    let entry = unsafe { libc::getpwnam(user_c.as_ptr()) };
    if entry.is_null() {
        return Err("Compte inconnu".into());
    }
    let entry = unsafe { &*entry };
    if entry.pw_name.is_null() || entry.pw_dir.is_null() || entry.pw_shell.is_null() {
        return Err("Informations de compte incomplètes".into());
    }
    Ok(LoginIdentity {
        username: unsafe { CStr::from_ptr(entry.pw_name) }
            .to_string_lossy()
            .into_owned(),
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        home: PathBuf::from(
            unsafe { CStr::from_ptr(entry.pw_dir) }
                .to_string_lossy()
                .into_owned(),
        ),
        shell: PathBuf::from(
            unsafe { CStr::from_ptr(entry.pw_shell) }
                .to_string_lossy()
                .into_owned(),
        ),
    })
}

fn process_is_single_threaded() -> Result<bool, String> {
    let tasks = std::fs::read_dir("/proc/self/task")
        .map_err(|error| format!("Impossible de vérifier les threads du superviseur: {error}"))?;
    let mut tasks = tasks.take(2);
    Ok(tasks.next().is_some() && tasks.next().is_none())
}

fn pass_pam_environment(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    matches!(
        name,
        "DBUS_SESSION_BUS_ADDRESS"
            | "LANG"
            | "LANGUAGE"
            | "TZ"
            | "XDG_SESSION_ID"
            | "XDG_SESSION_CLASS"
            | "XDG_SEAT"
            | "XDG_VTNR"
    ) || name.starts_with("LC_")
}

fn validate_runtime_directory(path: &std::path::Path, uid: u32) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("XDG_RUNTIME_DIR doit être un chemin absolu".into());
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("XDG_RUNTIME_DIR est indisponible: {error}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
    {
        return Err("XDG_RUNTIME_DIR doit appartenir à l'utilisateur et être privé".into());
    }
    Ok(())
}

fn combine_cleanup_results(
    first: Result<(), String>,
    second: Result<(), String>,
) -> Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(format!("{first}; {second}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn credential_validation_rejects_empty_and_control_input() {
        assert!(validate_credentials("romain", "secret", "bubblewm").is_ok());
        assert!(validate_credentials("", "secret", "bubblewm").is_err());
        assert!(validate_credentials("romain", "", "bubblewm").is_err());
        assert!(validate_credentials("romain\nroot", "secret", "bubblewm").is_err());
        assert!(validate_credentials("romain", "secret", "").is_err());
    }

    #[test]
    fn credential_validation_enforces_password_limit() {
        let password = "x".repeat(1025);
        assert!(validate_credentials("romain", &password, "bubblewm").is_err());
    }

    #[test]
    fn pam_environment_uses_a_narrow_allowlist() {
        for name in [
            "LANG",
            "LANGUAGE",
            "TZ",
            "XDG_SESSION_ID",
            "XDG_SESSION_CLASS",
            "XDG_SEAT",
            "XDG_VTNR",
            "LC_ALL",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            assert!(pass_pam_environment(std::ffi::OsStr::new(name)), "{name}");
        }

        for name in ["PATH", "HOME", "XDG_RUNTIME_DIR", "LD_PRELOAD", "DISPLAY"] {
            assert!(!pass_pam_environment(std::ffi::OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn runtime_directory_must_be_private_and_owned_by_expected_user() {
        let directory = tempfile::tempdir().expect("temporary directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("set private permissions");
        let uid = unsafe { libc::getuid() };
        assert!(validate_runtime_directory(directory.path(), uid).is_ok());
        assert!(validate_runtime_directory(directory.path(), uid.saturating_add(1)).is_err());

        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
            .expect("set public permissions");
        assert!(validate_runtime_directory(directory.path(), unsafe { libc::getuid() }).is_err());
    }

    #[test]
    fn runtime_directory_rejects_symlinks() {
        let parent = tempfile::tempdir().expect("temporary parent");
        let target = parent.path().join("target");
        std::fs::create_dir(&target).expect("create target");
        let link = parent.path().join("runtime-link");
        symlink(&target, &link).expect("create symlink");

        assert!(validate_runtime_directory(&link, unsafe { libc::getuid() }).is_err());
    }

    #[test]
    fn managed_process_is_terminated_when_supervisor_socket_closes() {
        use std::os::unix::process::CommandExt;
        use std::os::unix::process::ExitStatusExt;

        let child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn supervised process");
        let process_group = child.id() as libc::pid_t;
        let mut process = ManagedUserProcess::new(child);
        let (supervisor, mut worker) = UnixStream::pair().expect("control socket pair");
        drop(supervisor);

        let (status, supervisor_lost) = process
            .wait_while_supervised(
                &mut worker,
                Duration::from_millis(10),
                Duration::from_secs(1),
            )
            .expect("detect supervisor loss and terminate process");

        assert!(supervisor_lost);
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert_eq!(unsafe { libc::kill(-process_group, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn managed_process_is_terminated_on_unexpected_supervisor_data() {
        use std::io::Write;
        use std::os::unix::process::CommandExt;
        use std::os::unix::process::ExitStatusExt;

        let child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn supervised process");
        let mut process = ManagedUserProcess::new(child);
        let (mut supervisor, mut worker) =
            UnixStream::pair().expect("control socket pair");
        supervisor.write_all(b"unexpected").expect("write invalid protocol data");

        let (status, supervisor_lost) = process
            .wait_while_supervised(
                &mut worker,
                Duration::from_millis(10),
                Duration::from_secs(1),
            )
            .expect("reject unexpected control data");

        assert!(supervisor_lost);
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn managed_process_exits_normally_while_supervisor_channel_stays_open() {
        use std::os::unix::process::CommandExt;

        let child = Command::new("/bin/true")
            .process_group(0)
            .spawn()
            .expect("spawn short-lived process");
        let mut process = ManagedUserProcess::new(child);
        let (_supervisor, mut worker) = UnixStream::pair().expect("control socket pair");

        let (status, supervisor_lost) = process
            .wait_while_supervised(
                &mut worker,
                Duration::from_millis(10),
                Duration::from_secs(1),
            )
            .expect("wait for normal process exit");

        assert!(status.success());
        assert!(!supervisor_lost);
    }

    #[test]
    fn managed_process_terminate_sends_sigterm_and_reaps_leader() {
        use std::os::unix::process::CommandExt;
        use std::os::unix::process::ExitStatusExt;

        let child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let mut process = ManagedUserProcess::new(child);

        let status = process
            .terminate(Duration::from_secs(1))
            .expect("terminate managed process");

        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert_eq!(process.wait().expect("wait after terminate"), status);
    }

    #[test]
    fn managed_process_drop_kills_group_and_reaps_leader() {
        use std::os::unix::process::CommandExt;

        let child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let process_group = child.id() as libc::pid_t;
        let process = ManagedUserProcess::new(child);

        drop(process);

        assert_eq!(unsafe { libc::kill(-process_group, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    fn spawn_exited_leader_with_background_descendant()
    -> (ManagedUserProcess, libc::pid_t, tempfile::TempDir) {
        use std::os::unix::process::CommandExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_file = directory.path().join("descendant.pid");
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30 & echo $! > \"$PID_FILE\"; exit 0")
            .env("PID_FILE", &pid_file)
            .process_group(0)
            .spawn()
            .expect("spawn shell with background descendant");
        let mut process = ManagedUserProcess::new(child);
        assert!(process.wait().expect("wait for session leader").success());

        let descendant = std::fs::read_to_string(&pid_file)
            .expect("read descendant PID")
            .trim()
            .parse::<libc::pid_t>()
            .expect("parse descendant PID");
        assert_eq!(
            unsafe { libc::kill(descendant, 0) },
            0,
            "background descendant should outlive its leader"
        );

        (process, descendant, directory)
    }

    fn assert_process_stopped(pid: libc::pid_t) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(stat) => {
                    let state = stat
                        .rsplit_once(')')
                        .and_then(|(_, fields)| fields.split_whitespace().next());
                    if state == Some("Z") {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(error) => panic!("inspect descendant process: {error}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "descendant process {pid} is still running"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn managed_process_terminate_kills_descendants_after_leader_exits() {
        let (mut process, descendant, _directory) =
            spawn_exited_leader_with_background_descendant();

        // The direct child has already been reaped. Cleanup must still target
        // the process group so a background descendant cannot outlive the session.
        process
            .terminate(Duration::from_millis(100))
            .expect("terminate remaining session process group");

        assert_process_stopped(descendant);
    }

    #[test]
    fn managed_process_drop_kills_descendants_after_leader_exits() {
        let (process, descendant, _directory) = spawn_exited_leader_with_background_descendant();

        drop(process);

        assert_process_stopped(descendant);
    }
}
