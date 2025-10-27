pub mod error;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use create::{Notification, Request, Result};

use error::Error;

pub type RequestId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Version {
    #[serde(rename = "2.0")]
    V2_0,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEnvelope {
    pub jsonrpc: Version,
    #[serde(flatten)]
    pub message: Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Notification(NotificationMessage),
    Request(RequestMessage),
    Response(ResponseMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessage {
    pub id: RequestId,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMessage {
    pub id: RequestId,
    #[serde(flatten)]
    pub response: Response,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Response {
    Result(Result),
    Error(Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationMessage {
    #[serde(flatten)]
    pub notification: Notification,
}
