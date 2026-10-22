use std::{
    collections::VecDeque,
    ffi::{CStr, CString},
    fs,
    os::raw::{c_char, c_int, c_void},
    path::Path,
    ptr,
    sync::mpsc::{Receiver, Sender},
};

use anyhow::{bail, Context, Result};
use zeroize::Zeroize;

const PAM_SUCCESS: c_int = 0;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ESTABLISH_CRED: c_int = 0x0002;
const PAM_DELETE_CRED: c_int = 0x0004;
const PAM_TTY: c_int = 3;

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
    fn pam_set_item(handle: *mut PamHandle, item_type: c_int, item: *const c_void) -> c_int;
    fn pam_putenv(handle: *mut PamHandle, name_value: *const c_char) -> c_int;
    fn pam_getenv(handle: *mut PamHandle, name: *const c_char) -> *const c_char;
    fn pam_open_session(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_close_session(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_strerror(handle: *mut PamHandle, status: c_int) -> *const c_char;
}

pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(value: String) -> Result<Self> {
        Self::from_bytes(value.into_bytes())
    }

    pub fn from_bytes(value: Vec<u8>) -> Result<Self> {
        let secret = Self(value);
        if secret.0.contains(&0) {
            bail!("PAM response contains a NUL byte");
        }
        Ok(secret)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
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
        let mut conversation_data = Conversation::with_responses(responses);
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

    pub fn authenticate_interactive(
        service: &str,
        username: &str,
        prompts: Sender<Prompt>,
        responses: Receiver<Secret>,
    ) -> Result<Self> {
        let service = CString::new(service).context("invalid PAM service name")?;
        let username = CString::new(username).context("invalid PAM username")?;
        let mut conversation_data = Conversation::interactive(prompts, responses);
        let conversation = PamConversation {
            callback: conversation,
            data: (&mut conversation_data as *mut Conversation).cast(),
        };
        let mut handle = ptr::null_mut();
        check(
            handle,
            unsafe {
                pam_start(
                    service.as_ptr(),
                    username.as_ptr(),
                    &conversation,
                    &mut handle,
                )
            },
            "start PAM transaction",
        )?;
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
        if let Ok(active_tty) = fs::read_to_string("/sys/class/tty/tty0/active") {
            if let Some(tty) = active_session_tty(&active_tty) {
                let tty = CString::new(format!("/dev/{tty}"))?;
                check(
                    self.handle,
                    unsafe { pam_set_item(self.handle, PAM_TTY, tty.as_ptr().cast()) },
                    "set PAM session TTY",
                )?;
            }
        }
        self.set_session_environment()?;
        check(
            self.handle,
            unsafe { pam_open_session(self.handle, 0) },
            "open PAM session",
        )?;
        self.session_open = true;
        Ok(self)
    }

    pub fn session_environment(&self) -> Vec<(String, String)> {
        ["XDG_SESSION_ID", "XDG_SEAT", "XDG_VTNR"]
            .into_iter()
            .filter_map(|name| {
                let name_c = CString::new(name).expect("PAM environment variable name is valid");
                let value = unsafe { pam_getenv(self.handle, name_c.as_ptr()) };
                (!value.is_null()).then(|| {
                    (
                        name.to_owned(),
                        unsafe { CStr::from_ptr(value) }
                            .to_string_lossy()
                            .into_owned(),
                    )
                })
            })
            .collect()
    }

    fn set_session_environment(&self) -> Result<()> {
        for value in [
            "XDG_SESSION_CLASS=user",
            "XDG_SESSION_TYPE=wayland",
            "XDG_CURRENT_DESKTOP=Blair",
            "XDG_SESSION_DESKTOP=Blair",
        ] {
            let value = CString::new(value).expect("PAM session variable is valid");
            check(
                self.handle,
                unsafe { pam_putenv(self.handle, value.as_ptr()) },
                "set PAM session environment",
            )?;
        }
        Ok(())
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

fn active_session_tty(value: &str) -> Option<&str> {
    let tty = value.trim();
    let number = tty.strip_prefix("tty")?.parse::<u16>().ok()?;
    (number > 0).then_some(tty)
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

pub struct Prompt {
    pub message: String,
    pub echo: bool,
}

enum Responder {
    Static(VecDeque<Secret>),
    Interactive {
        prompts: Sender<Prompt>,
        responses: Receiver<Secret>,
    },
}

struct Conversation {
    responder: Responder,
}

impl Conversation {
    fn with_responses(responses: Vec<Secret>) -> Self {
        Self {
            responder: Responder::Static(responses.into()),
        }
    }

    fn interactive(prompts: Sender<Prompt>, responses: Receiver<Secret>) -> Self {
        Self {
            responder: Responder::Interactive { prompts, responses },
        }
    }

    fn response(&mut self, message: &PamMessage) -> Option<Secret> {
        match &mut self.responder {
            Responder::Static(responses) => responses.pop_front(),
            Responder::Interactive { prompts, responses } => {
                let prompt_message = if message.message.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(message.message) }
                        .to_string_lossy()
                        .into_owned()
                };
                prompts
                    .send(Prompt {
                        message: prompt_message,
                        echo: message.style == PAM_PROMPT_ECHO_ON,
                    })
                    .ok()?;
                responses.recv().ok()
            }
        }
    }
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
                free_responses(allocated, count as usize);
            }
            return PAM_CONV_ERR;
        }
        let response = unsafe { allocated.add(index) };
        match unsafe { (*message).style } {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                match conversation.response(unsafe { &*message }) {
                    Some(mut secret) => {
                        let length = secret.0.len();
                        let value = unsafe { libc::malloc(length + 1) as *mut c_char };
                        if value.is_null() {
                            secret.0.zeroize();
                            unsafe {
                                free_responses(allocated, count as usize);
                            }
                            return PAM_CONV_ERR;
                        }
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                secret.0.as_ptr(),
                                value.cast::<u8>(),
                                length,
                            );
                            *value.add(length) = 0;
                        }
                        secret.0.zeroize();
                        unsafe {
                            (*response).response = value;
                        }
                    }
                    None => {
                        unsafe {
                            free_responses(allocated, count as usize);
                        }
                        return PAM_CONV_ERR;
                    }
                }
            }
            _ => {}
        }
    }
    unsafe {
        *responses = allocated;
    }
    PAM_SUCCESS
}

unsafe fn free_responses(responses: *mut PamResponse, count: usize) {
    for index in 0..count {
        unsafe {
            libc::free((*responses.add(index)).response.cast());
        }
    }
    unsafe {
        libc::free(responses.cast());
    }
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

    #[test]
    fn packaged_service_registers_systemd_sessions() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/pam/alphin");
        let service = fs::read_to_string(path).unwrap();
        assert!(service.lines().any(|line| line.contains("pam_systemd.so")));
        assert!(service.lines().any(|line| line.contains("system-auth")));
    }

    #[test]
    fn accepts_only_a_real_virtual_terminal_for_the_session() {
        assert_eq!(active_session_tty("tty1\n"), Some("tty1"));
        assert_eq!(active_session_tty("tty12\n"), Some("tty12"));
        assert_eq!(active_session_tty("tty0\n"), None);
        assert_eq!(active_session_tty("pts/1\n"), None);
    }

    #[test]
    fn daemon_service_disables_core_dumps() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("packaging/systemd/alphind.service");
        let service = fs::read_to_string(path).unwrap();
        assert!(service.lines().any(|line| line == "LimitCORE=0"));
    }
}
