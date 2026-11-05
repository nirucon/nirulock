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
//! and aborts on any other prompt. Failing closed is deliberate: an unexpected
//! prompt must never unlock the screen. `PAM_ERROR_MSG` and `PAM_TEXT_INFO`
//! need no answer; their text is kept (sanitized, one line, at most 120
//! characters, never logged) so the prompt can show why an attempt failed,
//! for example `pam_faillock`'s "The account is locked due to 3 failed
//! logins." That is also why `pam_authenticate` is no longer called with
//! `PAM_SILENT`: modules such as pam_faillock stay quiet under it.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::Mutex;
use zeroize::Zeroize;

const PAM_SUCCESS: c_int = 0;
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
}

static PAM_LOCK: Mutex<()> = Mutex::new(());

/// What the conversation needs during one `pam_authenticate`, passed as the
/// conversation's `appdata_ptr`. The password is borrowed from the caller's
/// zeroizing buffer, so no process-wide copy exists.
struct Conversation<'a> {
    password: &'a [u8],
    /// Error and information texts of this attempt, sanitized.
    message: Option<String>,
}

/// The longest PAM message shown, in characters.
pub const MESSAGE_MAX: usize = 120;

/// One printable line of at most [`MESSAGE_MAX`] characters, or nothing.
///
/// Control characters, line breaks and invisible formatting characters
/// (bidirectional overrides, zero-width characters) become single spaces.
pub fn sanitize(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::new();
    let mut count = 0;
    let mut gap = false;
    for c in text.chars() {
        let invisible = matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}');
        if c.is_control() || c.is_whitespace() || invisible {
            gap = count > 0;
            continue;
        }
        if gap {
            if count + 1 >= MESSAGE_MAX {
                break;
            }
            out.push(' ');
            count += 1;
            gap = false;
        }
        out.push(c);
        count += 1;
        if count >= MESSAGE_MAX {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Keep `text` after the messages already received in this attempt.
fn remember(data: &mut Conversation, text: *const c_char) {
    if text.is_null() {
        return;
    }
    // SAFETY: PAM passes NUL-terminated message strings.
    let raw = unsafe { std::ffi::CStr::from_ptr(text) }.to_bytes();
    let joined = match (data.message.take(), sanitize(raw)) {
        (Some(old), Some(new)) => sanitize(format!("{old} {new}").as_bytes()),
        (old, new) => new.or(old),
    };
    data.message = joined;
}

/// The result of one attempt.
pub struct Outcome {
    /// True only for `PAM_SUCCESS`.
    pub accepted: bool,
    /// What PAM said during the attempt, if anything.
    pub message: Option<String>,
}

/// A `malloc`ed, NUL-terminated copy of `bytes` for a PAM response.
///
/// PAM owns the response once it is returned and frees it; Linux-PAM and
/// OpenPAM overwrite responses before freeing them (`_pam_drop_reply`). This
/// is the only copy made, so there is no intermediate `CString` to wipe.
unsafe fn response_copy(bytes: &[u8]) -> Option<*mut c_char> {
    if bytes.contains(&0) {
        return None;
    }
    let copy = libc::malloc(bytes.len() + 1) as *mut u8;
    if copy.is_null() {
        return None;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len());
    *copy.add(bytes.len()) = 0;
    Some(copy as *mut c_char)
}

/// Overwrite a NUL-terminated buffer in place.
unsafe fn wipe(text: *mut c_char) {
    if !text.is_null() {
        let len = libc::strlen(text);
        std::slice::from_raw_parts_mut(text as *mut u8, len).zeroize();
    }
}

/// Wipe and free the responses of an aborted conversation.
unsafe fn discard(out: *mut PamResponse, n: usize) {
    for i in 0..n {
        let response = (*out.add(i)).resp;
        if !response.is_null() {
            wipe(response);
            libc::free(response as *mut c_void);
        }
    }
    libc::free(out as *mut c_void);
}

/// PAM conversation: supply the password, ignore chatter, abort otherwise.
unsafe extern "C" fn conversation(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() {
        return PAM_CONV_ERR;
    }
    let mut data = (appdata as *mut Conversation).as_mut();
    let n = num_msg as usize;
    // calloc: every response starts as a null pointer with return code 0.
    let out = libc::calloc(n, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if out.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..n {
        let m = *msg.add(i);
        if m.is_null() {
            discard(out, n);
            return PAM_CONV_ERR;
        }
        match (*m).msg_style {
            PAM_PROMPT_ECHO_OFF => {
                let Some(copy) = data.as_ref().and_then(|data| response_copy(data.password)) else {
                    discard(out, n);
                    return PAM_CONV_ERR;
                };
                (*out.add(i)).resp = copy;
            }
            PAM_ERROR_MSG | PAM_TEXT_INFO => {
                // No reply needed; keep the text to show it.
                if let Some(data) = data.as_deref_mut() {
                    remember(data, (*m).msg);
                }
            }
            _ => {
                discard(out, n);
                return PAM_CONV_ERR;
            }
        }
    }
    *resp = out;
    PAM_SUCCESS
}

/// The account to authenticate: the owner of this process.
///
/// It comes from the password database for the real user id, never from
/// `$USER` or a fallback account: in a broken environment the old fallback
/// to `root` would have let the root password unlock a user's screen. A
/// failure is an error, and the lock refuses to start.
#[allow(dead_code)] // used by the lock screen binary only
pub fn current_user() -> Result<String, String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    user_for(unsafe { libc::getuid() })
}

/// The account name for `uid` through `getpwuid_r`.
#[allow(dead_code)] // used by the lock screen binary only
pub fn user_for(uid: libc::uid_t) -> Result<String, String> {
    let mut buf: Vec<c_char> = vec![0; 1024];
    loop {
        // SAFETY: a zeroed passwd is a valid out-parameter; the buffer and
        // its length match, and the strings it points to live in `buf`.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            let len = buf.len() * 2;
            buf.resize(len, 0);
            continue;
        }
        if rc != 0 || found.is_null() || pwd.pw_name.is_null() {
            return Err(format!("no account for user id {uid}"));
        }
        // SAFETY: pw_name is a NUL-terminated string inside `buf`.
        let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
        return match name.to_str() {
            Ok(name) if !name.is_empty() => Ok(name.to_string()),
            _ => Err(format!("unusable account name for user id {uid}")),
        };
    }
}

/// Verify `password` for `user`. Only `PAM_SUCCESS` is a success.
pub fn authenticate(user: &str, password: &str) -> Result<Outcome, String> {
    // One authentication at a time.
    let _guard = PAM_LOCK.lock().map_err(|_| "PAM state poisoned")?;
    let cuser = CString::new(user).map_err(|_| "invalid user name")?;
    let mut data = Conversation {
        password: password.as_bytes(),
        message: None,
    };
    let conv = PamConv {
        conv: Some(conversation),
        appdata_ptr: &mut data as *mut Conversation as *mut c_void,
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
        return Err(format!("PAM could not start (error {rc})"));
    }
    // `data` and `conv` outlive pam_end, the last call that may converse.
    let auth = unsafe { pam_authenticate(handle, 0) };
    unsafe {
        pam_end(handle, auth);
    }
    Ok(Outcome {
        accepted: auth == PAM_SUCCESS,
        message: data.message.take(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_user_comes_from_the_password_database() {
        let uid = unsafe { libc::getuid() };
        let user = current_user().unwrap();
        assert!(!user.is_empty());
        assert_eq!(user, user_for(uid).unwrap());
        // `$USER` is not trusted.
        std::env::set_var("USER", "nirulock-not-this-account");
        assert_eq!(current_user().unwrap(), user);
    }

    #[test]
    fn a_missing_account_is_an_error_not_a_fallback() {
        assert_eq!(user_for(0).unwrap(), "root");
        let missing = user_for(libc::uid_t::MAX - 7);
        assert!(missing.is_err(), "unexpected account {missing:?}");
    }

    #[test]
    fn wrong_password_never_authenticates() {
        // The contract: a wrong password must not unlock, and a missing PAM
        // must be an error rather than a success.
        if let Ok(outcome) = authenticate("nirulock-no-such-user", "definitely-wrong") {
            assert!(!outcome.accepted);
        }
    }

    #[test]
    fn response_copies_are_wiped_in_place() {
        unsafe {
            let copy = response_copy(b"fixture-secret").unwrap();
            assert_eq!(std::ffi::CStr::from_ptr(copy).to_bytes(), b"fixture-secret");
            let len = libc::strlen(copy);
            wipe(copy);
            let bytes = std::slice::from_raw_parts(copy as *const u8, len);
            assert!(bytes.iter().all(|b| *b == 0), "response not wiped");
            libc::free(copy as *mut c_void);
            assert!(response_copy(b"inner\0nul").is_none());
        }
    }

    #[test]
    fn conversation_answers_echo_off_with_the_password_only() {
        let prompt = PamMessage {
            msg_style: PAM_PROMPT_ECHO_OFF,
            msg: b"Password: \0".as_ptr() as *const c_char,
        };
        let mut msgs = [&prompt as *const PamMessage];
        let mut out: *mut PamResponse = std::ptr::null_mut();
        let mut data = Conversation {
            password: b"fixture",
            message: None,
        };
        let rc = unsafe {
            conversation(
                1,
                msgs.as_mut_ptr(),
                &mut out,
                &mut data as *mut Conversation as *mut c_void,
            )
        };
        assert_eq!(rc, PAM_SUCCESS);
        unsafe {
            assert_eq!(std::ffi::CStr::from_ptr((*out).resp).to_bytes(), b"fixture");
            discard(out, 1);
        }
        // Without conversation data there is no password to give: fail.
        let rc = unsafe { conversation(1, msgs.as_mut_ptr(), &mut out, std::ptr::null_mut()) };
        assert_ne!(rc, PAM_SUCCESS);
    }

    #[test]
    fn messages_are_captured_and_sanitized() {
        let error = PamMessage {
            msg_style: PAM_ERROR_MSG,
            msg: b"The account is locked due to 3 failed logins.\0".as_ptr() as *const c_char,
        };
        let info = PamMessage {
            msg_style: PAM_TEXT_INFO,
            msg: b"(10 minutes\nleft to unlock)\0".as_ptr() as *const c_char,
        };
        let prompt = PamMessage {
            msg_style: PAM_PROMPT_ECHO_OFF,
            msg: b"Password: \0".as_ptr() as *const c_char,
        };
        let mut data = Conversation {
            password: b"fixture",
            message: None,
        };
        let appdata = &mut data as *mut Conversation as *mut c_void;
        for m in [&error, &info, &prompt] {
            let mut msgs = [m as *const PamMessage];
            let mut out: *mut PamResponse = std::ptr::null_mut();
            let rc = unsafe { conversation(1, msgs.as_mut_ptr(), &mut out, appdata) };
            assert_eq!(rc, PAM_SUCCESS);
            unsafe { discard(out, 1) };
        }
        assert_eq!(
            data.message.as_deref(),
            Some("The account is locked due to 3 failed logins. (10 minutes left to unlock)")
        );
        // A prompt is never mistaken for a message.
        assert!(!data.message.unwrap().contains("Password"));
    }

    #[test]
    fn sanitizing_gives_one_short_printable_line() {
        assert_eq!(
            sanitize(b"  a\tb\r\n\x1b[31mc  ").as_deref(),
            Some("a b [31mc")
        );
        assert_eq!(
            sanitize("x\u{202e}y\u{200b}z".as_bytes()).as_deref(),
            Some("x y z")
        );
        assert_eq!(sanitize(b"\n\t "), None);
        assert_eq!(
            sanitize(b"\xff\xfeok").as_deref(),
            Some("\u{fffd}\u{fffd}ok")
        );
        let long = "word ".repeat(100);
        let short = sanitize(long.as_bytes()).unwrap();
        assert!(short.chars().count() <= MESSAGE_MAX);
        assert!(!short.ends_with(' '));
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
