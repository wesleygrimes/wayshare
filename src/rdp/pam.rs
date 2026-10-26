//! Linux-PAM authentication without opening a login session or changing credentials.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;

use anyhow::{Context, Result, ensure};
use zeroize::{Zeroize, Zeroizing};

#[repr(C)]
struct Message {
    style: c_int,
    text: *const c_char,
}

#[repr(C)]
struct Response {
    text: *mut c_char,
    code: c_int,
}

#[repr(C)]
struct Conv {
    callback: unsafe extern "C" fn(
        c_int,
        *const *const Message,
        *mut *mut Response,
        *mut c_void,
    ) -> c_int,
    data: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const Conv,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
}

struct Conversation {
    user: CString,
    password: Zeroizing<Vec<u8>>,
}

// PAM calls this synchronously while Conversation lives on authenticate's stack.
// Responses must be allocated with libc, because PAM frees them after ownership transfers.
unsafe extern "C" fn converse(
    count: c_int,
    messages: *const *const Message,
    response: *mut *mut Response,
    data: *mut c_void,
) -> c_int {
    const CONV_ERR: c_int = 19;
    if !(1..=32).contains(&count) || messages.is_null() || response.is_null() || data.is_null() {
        return CONV_ERR;
    }
    // SAFETY: PAM supplies count valid message pointers and an out-pointer. data is our
    // live, exclusively accessed Conversation. calloc initializes every response for cleanup.
    unsafe {
        *response = ptr::null_mut();
        let conversation = &*(data.cast::<Conversation>());
        let replies = libc::calloc(count as usize, size_of::<Response>()).cast::<Response>();
        if replies.is_null() {
            return 5; // PAM_BUF_ERR
        }
        for i in 0..count as usize {
            let message = *messages.add(i);
            let text = if message.is_null() {
                None
            } else {
                match (*message).style {
                    1 => Some(conversation.password.as_ptr().cast::<c_char>()), // ECHO_OFF
                    2 => Some(conversation.user.as_ptr()),                      // ECHO_ON
                    3 | 4 => Some(ptr::null()), // ERROR_MSG / TEXT_INFO, never log module text
                    _ => None,
                }
            };
            if let Some(text) = text {
                if text.is_null() {
                    continue;
                }
                (*replies.add(i)).text = libc::strdup(text);
                if !(*replies.add(i)).text.is_null() {
                    continue;
                }
            }
            // Ownership has not transferred on error. Wipe every allocated reply first.
            for j in 0..=i {
                let text = (*replies.add(j)).text;
                if !text.is_null() {
                    let len = libc::strlen(text);
                    std::slice::from_raw_parts_mut(text.cast::<u8>(), len).zeroize();
                    libc::free(text.cast());
                }
            }
            libc::free(replies.cast());
            return CONV_ERR;
        }
        *response = replies;
    }
    0
}

pub(super) fn service() -> &'static str {
    // Fall back only when a service file is absent, never after a failed login.
    if std::path::Path::new("/etc/pam.d/wayshare").exists() {
        "wayshare"
    } else if std::path::Path::new("/etc/pam.d/system-auth").exists() {
        "system-auth"
    } else {
        "login"
    }
}

pub(super) fn current_user() -> Result<String> {
    let mut buf = vec![0u8; 16384];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = ptr::null_mut();
    // SAFETY: All outputs point into live storage; getpwuid_r initializes entry on success.
    // Copy pw_name before dropping buf. Use the real UID, never the untrusted USER variable.
    unsafe {
        let status = libc::getpwuid_r(
            libc::getuid(),
            entry.as_mut_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut found,
        );
        ensure!(
            status == 0 && !found.is_null(),
            "cannot resolve the RDP account"
        );
        Ok(CStr::from_ptr(entry.assume_init().pw_name)
            .to_str()?
            .to_owned())
    }
}

pub(super) fn authenticate(service: &str, user: &str, password: &str) -> Result<bool> {
    let service = CString::new(service)?;
    let user = CString::new(user)?;
    let mut secret = Zeroizing::new(password.as_bytes().to_vec());
    ensure!(
        !secret.is_empty() && !secret.contains(&0),
        "invalid RDP password"
    );
    secret.push(0);
    let mut conversation = Conversation {
        user,
        password: secret,
    };
    let conv = Conv {
        callback: converse,
        data: (&mut conversation as *mut Conversation).cast(),
    };
    let mut handle = ptr::null_mut();
    // SAFETY: Strings and conversation storage remain valid through pam_end. The handle is
    // used on this thread only. No PAM session is opened and no process identity is changed.
    unsafe {
        let status = pam_start(
            service.as_ptr(),
            conversation.user.as_ptr(),
            &conv,
            &mut handle,
        );
        ensure!(status == 0, "PAM initialization failed ({status})");
        let mut status = pam_authenticate(handle, 0x8000 | 1); // SILENT | DISALLOW_NULL_AUTHTOK
        if status == 0 {
            status = pam_acct_mgmt(handle, 0x8000);
        }
        let end = pam_end(handle, status);
        ensure!(end == 0, "PAM cleanup failed ({end})");
        Ok::<bool, anyhow::Error>(status == 0)
    }
    .context("RDP PAM authentication")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_answers_and_rejects_unknown_prompts() {
        let mut data = Conversation {
            user: CString::new("test").unwrap(),
            password: Zeroizing::new(b"secret\0".to_vec()),
        };
        let mut reply = ptr::null_mut();
        let password = Message {
            style: 1,
            text: ptr::null(),
        };
        let unknown = Message {
            style: 99,
            text: ptr::null(),
        };
        // SAFETY: Valid local PAM-shaped storage, with libc ownership reclaimed below.
        unsafe {
            assert_eq!(
                converse(
                    1,
                    &(&password as *const _),
                    &mut reply,
                    (&mut data as *mut Conversation).cast()
                ),
                0
            );
            assert_eq!(CStr::from_ptr((*reply).text).to_bytes(), b"secret");
            std::slice::from_raw_parts_mut((*reply).text.cast::<u8>(), 6).zeroize();
            libc::free((*reply).text.cast());
            libc::free(reply.cast());
            assert_eq!(
                converse(
                    1,
                    &(&unknown as *const _),
                    &mut reply,
                    (&mut data as *mut Conversation).cast()
                ),
                19
            );
            assert!(reply.is_null());
        }
    }
}
