//! Versioned wire protocol shared by BubbleWM and its companion processes.
//!
//! The transport is a local Unix-domain stream. Messages are UTF-8 JSON frames
//! terminated by a newline and bounded by MAX_FRAME_SIZE. Transport setup,
//! peer authentication and request dispatch belong to the owning application.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[cfg(target_os = "linux")]
pub mod unix;

/// Current IPC protocol version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Maximum serialized message size, excluding the terminating newline.
pub const MAX_FRAME_SIZE: usize = 64 * 1024;

/// Request envelope. The request ID allows clients to correlate replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol_version: u16,
    pub request_id: u64,
    pub request: Request,
}

/// Commands accepted by the compositor control endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    GetCapabilities,
    GetWorkspaceState,
    GetThemePalette,
    WorkspacePrevious,
    WorkspaceNext,
    ActivateWorkspace { index: u32 },
    GetSessionState,
    GetSessionCapabilities,
    RequestSessionAction { action: SessionAction },
}

/// Actions that a session manager may request from a native session provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    Lock,
    Logout,
    Suspend,
    Reboot,
    PowerOff,
}

/// Runtime kind reported by the session manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEnvironment {
    NestedDevelopment,
    Native,
}

/// Explicit capability flags; unsupported actions must remain false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCapabilities {
    pub lock: bool,
    pub logout: bool,
    pub suspend: bool,
    pub reboot: bool,
    pub power_off: bool,
}

/// Session status reported to shell and settings clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub environment: SessionEnvironment,
    pub action_in_progress: bool,
}

/// Response envelope matching a request ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol_version: u16,
    pub request_id: u64,
    pub response: Response,
}

/// Explicit success or failure; protocol errors are never encoded as success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Success { data: ResponseData },
    Failure { error: ErrorResponse },
}

/// Versioned semantic colors shared with shell clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemePalette {
    pub schema_version: u32,
    pub generation: u64,
    pub primary: [u8; 3],
    pub primary_container: [u8; 3],
    pub on_primary_container: [u8; 3],
    pub surface: [u8; 3],
    pub on_surface: [u8; 3],
    pub surface_variant: [u8; 3],
    pub outline: [u8; 3],
    pub warning: [u8; 3],
    pub success: [u8; 3],
    pub error: [u8; 3],
}

/// Data returned by supported read or control requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseData {
    Capabilities { workspace_control: bool },
    WorkspaceState { active: u32, count: u32 },
    ThemePalette { palette: Option<ThemePalette> },
    SessionCapabilities { capabilities: SessionCapabilities },
    SessionState { state: SessionState },
    Acknowledged,
}

/// Stable machine-readable error category with a user-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedCommand,
    UnsupportedVersion,
    Unavailable,
    Internal,
}

/// Structured failure information returned by the compositor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
}

/// Errors encountered while validating or decoding protocol frames.
#[derive(Debug)]
pub enum ProtocolError {
    UnsupportedVersion(u16),
    FrameTooLarge,
    UnterminatedFrame,
    Io(io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported BubbleWM IPC protocol version: {version}"
                )
            }
            Self::FrameTooLarge => write!(formatter, "BubbleWM IPC frame exceeds the size limit"),
            Self::UnterminatedFrame => {
                write!(formatter, "BubbleWM IPC frame is not newline-terminated")
            }
            Self::Io(error) => write!(formatter, "BubbleWM IPC transport error: {error}"),
            Self::Json(error) => write!(formatter, "invalid BubbleWM IPC JSON: {error}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Rejects a message version that this implementation cannot safely interpret.
pub fn validate_version(version: u16) -> Result<(), ProtocolError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::UnsupportedVersion(version))
    }
}

/// Reads exactly one bounded newline-delimited JSON frame.
pub fn read_frame<R, T>(reader: &mut R) -> Result<T, ProtocolError>
where
    R: BufRead,
    T: DeserializeOwned,
{
    let mut bytes = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(ProtocolError::UnterminatedFrame);
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk_len = newline.map_or(available.len(), |position| position + 1);
        if bytes.len() + chunk_len > MAX_FRAME_SIZE + 1 {
            return Err(ProtocolError::FrameTooLarge);
        }

        bytes.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);
        if newline.is_some() {
            break;
        }
    }
    bytes.pop();
    serde_json::from_slice(&bytes).map_err(ProtocolError::from)
}

/// Writes one JSON frame and flushes it so a waiting peer can process the request.
pub fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), ProtocolError>
where
    W: Write,
    T: Serialize,
{
    let bytes = serde_json::to_vec(message)?;
    if bytes.len() > MAX_FRAME_SIZE {
        return Err(ProtocolError::FrameTooLarge);
    }
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn request() -> RequestEnvelope {
        RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: 42,
            request: Request::WorkspaceNext,
        }
    }

    #[test]
    fn request_round_trips_as_a_single_json_frame() {
        let expected = request();
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &expected).expect("frame should serialize");
        let decoded: RequestEnvelope =
            read_frame(&mut Cursor::new(bytes)).expect("frame should deserialize");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn theme_palette_response_round_trips_all_semantic_roles() {
        let expected = ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: 103,
            response: Response::Success {
                data: ResponseData::ThemePalette {
                    palette: Some(ThemePalette {
                        schema_version: 1,
                        generation: 9,
                        primary: [12, 34, 56],
                        primary_container: [20, 40, 60],
                        on_primary_container: [240, 245, 250],
                        surface: [18, 22, 30],
                        on_surface: [230, 235, 245],
                        surface_variant: [38, 44, 55],
                        outline: [90, 100, 115],
                        warning: [240, 180, 40],
                        success: [40, 190, 120],
                        error: [220, 60, 70],
                    }),
                },
            },
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &expected).expect("theme palette should serialize");
        let decoded: ResponseEnvelope =
            read_frame(&mut Cursor::new(bytes)).expect("theme palette should deserialize");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn session_action_requests_round_trip_with_typed_actions() {
        let expected = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: 77,
            request: Request::RequestSessionAction {
                action: SessionAction::PowerOff,
            },
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &expected).expect("session request should serialize");
        let decoded: RequestEnvelope =
            read_frame(&mut Cursor::new(bytes)).expect("session request should deserialize");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn rejects_unsupported_protocol_versions() {
        assert!(matches!(
            validate_version(PROTOCOL_VERSION + 1),
            Err(ProtocolError::UnsupportedVersion(_))
        ));
        assert!(validate_version(PROTOCOL_VERSION).is_ok());
    }

    #[test]
    fn rejects_unterminated_frames() {
        let mut input = Cursor::new(br#"{"protocol_version":1}"#.to_vec());
        assert!(matches!(
            read_frame::<_, RequestEnvelope>(&mut input),
            Err(ProtocolError::UnterminatedFrame)
        ));
    }

    #[test]
    fn rejects_frames_over_the_limit_without_unbounded_reading() {
        let mut input = Cursor::new(vec![b'a'; MAX_FRAME_SIZE + 2]);
        assert!(matches!(
            read_frame::<_, RequestEnvelope>(&mut input),
            Err(ProtocolError::UnterminatedFrame | ProtocolError::FrameTooLarge)
        ));
    }

    #[test]
    fn response_preserves_request_id_and_structured_error() {
        let response = ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: 42,
            response: Response::Failure {
                error: ErrorResponse {
                    code: ErrorCode::Unavailable,
                    message: "workspace manager is not ready".into(),
                },
            },
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &response).expect("response should serialize");
        let decoded: ResponseEnvelope =
            read_frame(&mut Cursor::new(bytes)).expect("response should deserialize");
        assert_eq!(decoded, response);
    }
}
