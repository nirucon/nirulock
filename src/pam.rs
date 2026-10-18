//! PAM authentication, loaded at runtime.
//!
//! NIRULOCK verifies the password with the system's PAM stack, like `slock`:
//! that is the only way to respect the account rules already configured on the
//! machine (password aging, locked accounts, `pam_unix`, fingerprint modules).
//!
//! NIRULOCK links libpam directly. That is deliberate and different from the
//! other NIRU programs, which are static: PAM is a *system* API that loads
//! modules from /lib/security and reads /etc/pam.d. A statically linked binary
//! that `dlopen`s the system libpam ends up with two libc instances and the
//! conversation callback fails silently, which would look like a wrong
//! password. `slock` links PAM the same way. The release therefore ships a
//! glibc dynamic binary for NIRULOCK, and PAM is present on every supported
//! system (it is a dependency of login and sudo).
//!
//! The conversation callback answers `PAM_PROMPT_ECHO_OFF` with the password
//! and aborts on anything else. Failing closed is deliberate: an unexpected
//! prompt must never unlock the screen.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::Mutex;

const PAM_SUCCESS: c_int = 0;
const PAM_SILENT: c_int = 0x8000;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
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
struct PamConv {
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

#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        handle: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(handle: *mut PamHandle, status: c_int) -> c_int;
    fn calloc(n: usize, size: usize) -> *mut c_void;
    fn free(p: *mut c_void);
    fn strdup(s: *const c_char) -> *mut c_char;
}

static PAM_LOCK: Mutex<()> = Mutex::new(());
static PASSWORD: Mutex<Option<String>> = Mutex::new(None);

/// PAM conversation: supply the password, ignore chatter, abort otherwise.
unsafe extern "C" fn conversation(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    _appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() {
        return PAM_CONV_ERR;
    }
    let password = PASSWORD.lock().ok().and_then(|p| p.clone());
    let n = num_msg as usize;
    let out = calloc(n, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if out.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..n {
        let m = *msg.add(i);
        if m.is_null() {
            free(out as *mut c_void);
            return PAM_CONV_ERR;
        }
        (*out.add(i)).resp = std::ptr::null_mut();
        (*out.add(i)).resp_retcode = 0;
        match (*m).msg_style {
            PAM_PROMPT_ECHO_OFF => {
                let Some(pw) = password.as_deref() else {
                    free(out as *mut c_void);
                    return PAM_CONV_ERR;
                };
                let Ok(c) = CString::new(pw) else {
                    free(out as *mut c_void);
                    return PAM_CONV_ERR;
                };
                let dup = strdup(c.as_ptr());
                if dup.is_null() {
                    free(out as *mut c_void);
                    return PAM_CONV_ERR;
                }
                (*out.add(i)).resp = dup;
            }
            PAM_ERROR_MSG | PAM_TEXT_INFO => { /* no reply needed */ }
            _ => {
                free(out as *mut c_void);
                return PAM_CONV_ERR;
            }
        }
    }
    *resp = out;
    PAM_SUCCESS
}

/// The account to authenticate: the owner of this session.
#[allow(dead_code)] // used by the lock screen binary only
pub fn current_user() -> String {
    if let Ok(u) = std::env::var("USER") {
        if !u.is_empty() {
            return u;
        }
    }
    if let Ok(out) = std::process::Command::new("id").arg("-un").output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    "root".to_string()
}

/// Verify `password` for `user`. Only `PAM_SUCCESS` is a success.
pub fn authenticate(user: &str, password: &str) -> Result<bool, String> {
    if let Ok(mut p) = PASSWORD.lock() {
        *p = Some(password.to_string());
    }
    let result = {
        // One authentication at a time: the conversation callback reads a
        // process-wide password.
        let _guard = PAM_LOCK.lock().map_err(|_| "PAM state poisoned")?;
        let cuser = CString::new(user).map_err(|_| "invalid user name")?;
        let conv = PamConv {
            conv: Some(conversation),
            appdata_ptr: std::ptr::null_mut(),
        };
        // Prefer the dedicated `nirulock` service the installer provides.
        // It uses the authentication stack only; `login` on Void additionally
        // requires a shell in /etc/shells and refuses when /etc/nologin
        // exists, which made a locked screen impossible to unlock. `login`
        // stays as a fallback so a missing service file never locks the user
        // out of their own session.
        let mut handle: *mut PamHandle = std::ptr::null_mut();
        let mut rc = PAM_SUCCESS;
        for service in ["nirulock", "login"] {
            let svc = CString::new(service).unwrap();
            rc = unsafe { pam_start(svc.as_ptr(), cuser.as_ptr(), &conv, &mut handle) };
            if rc == PAM_SUCCESS && !handle.is_null() {
                break;
            }
            handle = std::ptr::null_mut();
        }
        if rc != PAM_SUCCESS || handle.is_null() {
            Err(format!("PAM could not start (error {rc})"))
        } else {
            let auth = unsafe { pam_authenticate(handle, PAM_SILENT) };
            unsafe {
                pam_end(handle, auth);
            }
            Ok(auth == PAM_SUCCESS)
        }
    };
    // Never keep the password around after the attempt.
    if let Ok(mut p) = PASSWORD.lock() {
        *p = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_is_not_empty() {
        assert!(!current_user().is_empty());
    }

    #[test]
    fn wrong_password_never_authenticates() {
        // The contract: a wrong password must not unlock, and a missing PAM
        // must be an error rather than a success.
        if let Ok(ok) = authenticate("nirulock-no-such-user", "definitely-wrong") {
            assert!(!ok);
        }
    }

    #[test]
    fn conversation_aborts_on_a_null_message() {
        let mut msgs = [std::ptr::null::<PamMessage>(); 1];
        let mut out: *mut PamResponse = std::ptr::null_mut();
        let rc = unsafe { conversation(1, msgs.as_mut_ptr(), &mut out, std::ptr::null_mut()) };
        assert_ne!(rc, PAM_SUCCESS);
    }

    #[test]
    fn conversation_aborts_on_unknown_prompt() {
        let m = PamMessage {
            msg_style: 99,
            msg: std::ptr::null(),
        };
        let mut msgs = [&m as *const PamMessage];
        let mut out: *mut PamResponse = std::ptr::null_mut();
        let rc = unsafe { conversation(1, msgs.as_mut_ptr(), &mut out, std::ptr::null_mut()) };
        assert_ne!(rc, PAM_SUCCESS);
    }
}
