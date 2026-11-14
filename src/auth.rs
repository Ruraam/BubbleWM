//! PAM authentication for the built-in lock screen.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr,
};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_CONV_ERR: c_int = 19;

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
            unsafe { libc::free(response as *mut c_void) };
            return PAM_CONV_ERR;
        }
        let style = unsafe { (*message).msg_style };
        let destination = unsafe { response.add(index) };
        match style {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                let copied = unsafe { libc::strdup(password.as_ptr()) };
                if copied.is_null() {
                    unsafe { libc::free(response as *mut c_void) };
                    return PAM_CONV_ERR;
                }
                unsafe { (*destination).resp = copied };
                unsafe { (*destination).resp_retcode = 0 };
            }
            PAM_ERROR_MSG | PAM_TEXT_INFO => {
                unsafe { (*destination).resp = ptr::null_mut() };
                unsafe { (*destination).resp_retcode = 0 };
            }
            _ => {
                unsafe { libc::free(response as *mut c_void) };
                return PAM_CONV_ERR;
            }
        }
    }
    unsafe { *responses = response };
    PAM_SUCCESS
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
    if password.is_empty() {
        return Err("Mot de passe vide".into());
    }
    if password.len() > 1024 {
        return Err("Mot de passe trop long".into());
    }
    let user = current_user().ok_or_else(|| "Utilisateur inconnu".to_owned())?;
    let user_c = CString::new(user).map_err(|_| "Identifiant invalide".to_owned())?;
    let service_c = CString::new(service).map_err(|_| "Service PAM invalide".to_owned())?;
    let password_c = CString::new(password).map_err(|_| "Mot de passe invalide".to_owned())?;

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
        return Err(format!("PAM initialisation ({status})"));
    }

    let authenticated = unsafe { pam_authenticate(handle, 0) };
    let account = if authenticated == PAM_SUCCESS {
        unsafe { pam_acct_mgmt(handle, 0) }
    } else {
        authenticated
    };
    unsafe { pam_end(handle, account) };

    if authenticated != PAM_SUCCESS {
        return Err("Mot de passe incorrect".into());
    }
    if account != PAM_SUCCESS {
        return Err("Compte verrouillé ou expiré".into());
    }
    Ok(())
}
