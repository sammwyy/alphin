use std::{
    collections::VecDeque,
    ffi::{CStr, CString},
    os::raw::{c_char, c_int, c_void},
    path::Path,
    ptr,
};

use anyhow::{bail, Context, Result};
use zeroize::Zeroize;

const PAM_SUCCESS: c_int = 0;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ESTABLISH_CRED: c_int = 0x0002;
const PAM_DELETE_CRED: c_int = 0x0004;

#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct PamMessage {
    style: c_int,
    message: *const c_char,
}

#[repr(C)]
struct PamResponse {
    response: *mut c_char,
    code: c_int,
}

#[repr(C)]
struct PamConversation {
    callback: unsafe extern "C" fn(
        c_int,
        *mut *const PamMessage,
        *mut *mut PamResponse,
        *mut c_void,
    ) -> c_int,
    data: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conversation: *const PamConversation,
        handle: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conversation: *const PamConversation,
        directory: *const c_char,
        handle: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_end(handle: *mut PamHandle, status: c_int) -> c_int;
    fn pam_authenticate(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_setcred(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_open_session(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_close_session(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_strerror(handle: *mut PamHandle, status: c_int) -> *const c_char;
}

pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(value: String) -> Result<Self> {
        if value.as_bytes().contains(&0) {
            bail!("PAM response contains a NUL byte");
        }
        Ok(Self(value.into_bytes()))
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

pub struct Transaction {
    handle: *mut PamHandle,
    session_open: bool,
    credentials_established: bool,
}

impl Transaction {
    pub fn authenticate(service: &str, username: &str, responses: Vec<Secret>) -> Result<Self> {
        Self::authenticate_in(service, username, responses, None)
    }

    pub fn authenticate_in(
        service: &str,
        username: &str,
        responses: Vec<Secret>,
        config_dir: Option<&Path>,
    ) -> Result<Self> {
        let service = CString::new(service).context("invalid PAM service name")?;
        let username = CString::new(username).context("invalid PAM username")?;
        let mut conversation_data = Conversation {
            responses: responses.into(),
        };
        let conversation = PamConversation {
            callback: conversation,
            data: (&mut conversation_data as *mut Conversation).cast(),
        };
        let mut handle = ptr::null_mut();
        let status = match config_dir {
            Some(directory) => {
                let directory = CString::new(directory.as_os_str().as_encoded_bytes())
                    .context("invalid PAM configuration directory")?;
                unsafe {
                    pam_start_confdir(
                        service.as_ptr(),
                        username.as_ptr(),
                        &conversation,
                        directory.as_ptr(),
                        &mut handle,
                    )
                }
            }
            None => unsafe {
                pam_start(
                    service.as_ptr(),
                    username.as_ptr(),
                    &conversation,
                    &mut handle,
                )
            },
        };
        check(handle, status, "start PAM transaction")?;
        let transaction = Self {
            handle,
            session_open: false,
            credentials_established: false,
        };
        transaction.run_authentication()?;
        Ok(transaction)
    }

    pub fn open_session(mut self) -> Result<Self> {
        check(
            self.handle,
            unsafe { pam_setcred(self.handle, PAM_ESTABLISH_CRED) },
            "establish PAM credentials",
        )?;
        self.credentials_established = true;
        check(
            self.handle,
            unsafe { pam_open_session(self.handle, 0) },
            "open PAM session",
        )?;
        self.session_open = true;
        Ok(self)
    }

    fn run_authentication(&self) -> Result<()> {
        check(
            self.handle,
            unsafe { pam_authenticate(self.handle, 0) },
            "authenticate PAM user",
        )?;
        check(
            self.handle,
            unsafe { pam_acct_mgmt(self.handle, 0) },
            "validate PAM account",
        )
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        if self.session_open {
            unsafe {
                pam_close_session(self.handle, 0);
            }
        }
        if self.credentials_established {
            unsafe {
                pam_setcred(self.handle, PAM_DELETE_CRED);
            }
        }
        unsafe {
            pam_end(self.handle, PAM_SUCCESS);
        }
    }
}

struct Conversation {
    responses: VecDeque<Secret>,
}

unsafe extern "C" fn conversation(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    data: *mut c_void,
) -> c_int {
    if count < 0 || messages.is_null() || responses.is_null() || data.is_null() {
        return PAM_CONV_ERR;
    }
    let conversation = unsafe { &mut *data.cast::<Conversation>() };
    let allocated = unsafe {
        libc::calloc(count as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse
    };
    if allocated.is_null() {
        return PAM_CONV_ERR;
    }
    for index in 0..count as usize {
        let message = unsafe { *messages.add(index) };
        if message.is_null() {
            unsafe {
                libc::free(allocated.cast());
            }
            return PAM_CONV_ERR;
        }
        let response = unsafe { allocated.add(index) };
        match unsafe { (*message).style } {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => match conversation.responses.pop_front() {
                Some(mut secret) => {
                    let value = match CString::new(secret.0.as_slice()) {
                        Ok(value) => value,
                        Err(_) => {
                            secret.0.zeroize();
                            unsafe {
                                libc::free(allocated.cast());
                            }
                            return PAM_CONV_ERR;
                        }
                    };
                    secret.0.zeroize();
                    unsafe {
                        (*response).response = libc::strdup(value.as_ptr());
                    }
                    if unsafe { (*response).response }.is_null() {
                        unsafe {
                            libc::free(allocated.cast());
                        }
                        return PAM_CONV_ERR;
                    }
                }
                None => {
                    unsafe {
                        libc::free(allocated.cast());
                    }
                    return PAM_CONV_ERR;
                }
            },
            _ => {}
        }
    }
    unsafe {
        *responses = allocated;
    }
    PAM_SUCCESS
}

fn check(handle: *mut PamHandle, status: c_int, operation: &str) -> Result<()> {
    if status == PAM_SUCCESS {
        return Ok(());
    }
    let message = if handle.is_null() {
        format!("PAM status {status}")
    } else {
        let value = unsafe { pam_strerror(handle, status) };
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    };
    bail!("{operation}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn authenticates_with_an_isolated_permit_service() {
        let directory = std::env::temp_dir().join(format!(
            "alphin-pam-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("alphin-test"), "auth required pam_permit.so\naccount required pam_permit.so\nsession required pam_permit.so\n").unwrap();
        Transaction::authenticate_in("alphin-test", "test-user", Vec::new(), Some(&directory))
            .unwrap()
            .open_session()
            .unwrap();
        fs::remove_file(directory.join("alphin-test")).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn rejects_secret_nul_bytes() {
        assert!(Secret::new("bad\0secret".into()).is_err());
    }
}
