use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Error {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            message: code.default_message().to_string(),
            code,
            data: None,
        }
    }

    pub fn with_message(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(code: ErrorCode, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i32", try_from = "i32")]
pub enum ErrorCode {
    // JSON-RPC 2.0 Standards (-32768 to -32000)
    ParseError = -32700, // Invalid JSON was received by the server. An error occurred on the server while parsing the JSON text.
    InvalidRequest = -32600, // The JSON sent is not a valid Request object.
    MethodNotFound = -32601, // The method does not exist / is not available.
    InvalidParams = -32602, // Invalid method parameter(s).
    InternalError = -32603, // Internal JSON-RPC error.

    // Application (-32000 to -32099)
    Unauthorized = -32401,
    Forbidden = -32403,
    NotFound = -32404,
    Conflict = -32409,
    InternalServerError = -32500,
}

impl From<ErrorCode> for i32 {
    fn from(code: ErrorCode) -> i32 {
        code as i32
    }
}

impl TryFrom<i32> for ErrorCode {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            -32700 => Ok(ErrorCode::ParseError),
            -32600 => Ok(ErrorCode::InvalidRequest),
            -32601 => Ok(ErrorCode::MethodNotFound),
            -32602 => Ok(ErrorCode::InvalidParams),
            -32603 => Ok(ErrorCode::InternalError),
            -32401 => Ok(ErrorCode::Unauthorized),
            -32403 => Ok(ErrorCode::Forbidden),
            -32404 => Ok(ErrorCode::NotFound),
            -32409 => Ok(ErrorCode::Conflict),
            -32500 => Ok(ErrorCode::InternalServerError),
            _ => Err(format!("Unknown error code: {}", value)),
        }
    }
}

impl ErrorCode {
    pub fn default_message(&self) -> &'static str {
        match self {
            ErrorCode::ParseError => "Parse error",
            ErrorCode::InvalidRequest => "Invalid request",
            ErrorCode::MethodNotFound => "Method not found",
            ErrorCode::InvalidParams => "Invalid params",
            ErrorCode::InternalError => "Internal error",
            ErrorCode::Unauthorized => "Unauthorized",
            ErrorCode::Forbidden => "Forbidden",
            ErrorCode::NotFound => "Not found",
            ErrorCode::Conflict => "Conflict",
            ErrorCode::InternalServerError => "Internal server error",
        }
    }
}
