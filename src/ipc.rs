use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    GetState { version: u16 },
    ListUsers { version: u16 },
    ListSessions { version: u16 },
    Status { version: u16 },
    BeginAuthentication { version: u16, username: String },
    Respond { version: u16, response: String },
    Cancel { version: u16 },
    StartSession { version: u16, session: String },
    PowerAction { version: u16, action: String },
}

impl Request {
    pub fn version(&self) -> u16 {
        match self {
            Self::GetState { version }
            | Self::ListUsers { version }
            | Self::ListSessions { version }
            | Self::Status { version }
            | Self::BeginAuthentication { version, .. }
            | Self::Respond { version, .. }
            | Self::Cancel { version }
            | Self::StartSession { version, .. }
            | Self::PowerAction { version, .. } => *version,
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    State {
        version: u16,
        phase: &'static str,
    },
    Sessions {
        version: u16,
        sessions: Vec<SessionInfo>,
    },
    Users {
        version: u16,
        users: Vec<UserInfo>,
    },
    Status {
        version: u16,
        status: &'static str,
    },
    Error {
        version: u16,
        code: &'static str,
    },
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct UserInfo {
    pub username: String,
    pub display_name: String,
}
