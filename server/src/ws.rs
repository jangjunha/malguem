use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::auth::user_for_token;
use crate::error::ApiError;
use crate::spaces::{channel_space, member_ids, space_role};
use crate::state::SharedState;

/// In-memory registry of live WebSocket connections and per-channel call
/// rosters. All state here is ephemeral by design: a server restart simply
/// drops calls, and clients rejoin.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<HubInner>,
}

#[derive(Default)]
struct HubInner {
    next_conn: u64,
    conns: HashMap<String, Vec<(u64, mpsc::UnboundedSender<String>)>>,
    calls: HashMap<String, HashSet<String>>,
    /// channel → participants currently sharing their screen.
    live: HashMap<String, HashSet<String>>,
}

impl Hub {
    fn register(&self, user_id: &str) -> (u64, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut g = self.inner.lock().unwrap();
        g.next_conn += 1;
        let id = g.next_conn;
        g.conns.entry(user_id.to_string()).or_default().push((id, tx));
        (id, rx)
    }

    /// Returns true when this was the user's last connection.
    fn unregister(&self, user_id: &str, conn_id: u64) -> bool {
        let mut g = self.inner.lock().unwrap();
        if let Some(list) = g.conns.get_mut(user_id) {
            list.retain(|(id, _)| *id != conn_id);
            if list.is_empty() {
                g.conns.remove(user_id);
                return true;
            }
            return false;
        }
        true
    }

    pub fn send_to(&self, user_id: &str, event: &Value) {
        self.send_many(std::slice::from_ref(&user_id.to_string()), event);
    }

    /// Send to one specific connection of a user (e.g. a snapshot on connect).
    fn send_to_conn(&self, user_id: &str, conn_id: u64, event: &Value) {
        let g = self.inner.lock().unwrap();
        if let Some(list) = g.conns.get(user_id) {
            for (id, tx) in list {
                if *id == conn_id {
                    let _ = tx.send(event.to_string());
                }
            }
        }
    }

    pub fn send_many(&self, users: &[String], event: &Value) {
        let text = event.to_string();
        let g = self.inner.lock().unwrap();
        for u in users {
            if let Some(list) = g.conns.get(u) {
                for (_, tx) in list {
                    let _ = tx.send(text.clone());
                }
            }
        }
    }

    fn call_join(&self, channel_id: &str, user_id: &str) -> Vec<String> {
        let mut g = self.inner.lock().unwrap();
        let roster = g.calls.entry(channel_id.to_string()).or_default();
        roster.insert(user_id.to_string());
        roster.iter().cloned().collect()
    }

    fn call_leave(&self, channel_id: &str, user_id: &str) -> Option<Vec<String>> {
        let mut g = self.inner.lock().unwrap();
        if let Some(live) = g.live.get_mut(channel_id) {
            live.remove(user_id);
        }
        let roster = g.calls.get_mut(channel_id)?;
        if !roster.remove(user_id) {
            return None;
        }
        let remaining: Vec<String> = roster.iter().cloned().collect();
        if remaining.is_empty() {
            g.calls.remove(channel_id);
            g.live.remove(channel_id);
        }
        Some(remaining)
    }

    fn leave_all(&self, user_id: &str) -> Vec<(String, Vec<String>)> {
        let mut g = self.inner.lock().unwrap();
        g.live.retain(|_, set| {
            set.remove(user_id);
            !set.is_empty()
        });
        let mut out = Vec::new();
        g.calls.retain(|channel, roster| {
            if roster.remove(user_id) {
                out.push((channel.clone(), roster.iter().cloned().collect()));
            }
            !roster.is_empty()
        });
        out
    }

    /// Mark a call participant as (not) sharing their screen. Returns false if
    /// they aren't in the call.
    fn set_streaming(&self, channel_id: &str, user_id: &str, streaming: bool) -> bool {
        let mut g = self.inner.lock().unwrap();
        if !g.calls.get(channel_id).is_some_and(|r| r.contains(user_id)) {
            return false;
        }
        let live = g.live.entry(channel_id.to_string()).or_default();
        if streaming {
            live.insert(user_id.to_string());
        } else {
            live.remove(user_id);
        }
        if live.is_empty() {
            g.live.remove(channel_id);
        }
        true
    }

    /// Who is in a channel's call and who of them is streaming (sorted).
    fn presence(&self, channel_id: &str) -> (Vec<String>, Vec<String>) {
        let g = self.inner.lock().unwrap();
        let sorted = |set: Option<&HashSet<String>>| {
            let mut v: Vec<String> = set.map(|s| s.iter().cloned().collect()).unwrap_or_default();
            v.sort();
            v
        };
        (sorted(g.calls.get(channel_id)), sorted(g.live.get(channel_id)))
    }

    fn active_calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.keys().cloned().collect()
    }

    fn in_call(&self, channel_id: &str, user_id: &str) -> bool {
        let g = self.inner.lock().unwrap();
        g.calls
            .get(channel_id)
            .map(|r| r.contains(user_id))
            .unwrap_or(false)
    }
}

#[derive(Deserialize)]
pub struct WsQuery {
    token: Option<String>,
}

pub async fn ws_handler(
    State(state): State<SharedState>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // Token via `Sec-WebSocket-Protocol: bearer, <token>` (browser WebSocket
    // can't set Authorization) or `?token=` as a dev convenience.
    let proto_token = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let parts: Vec<&str> = v.split(',').map(str::trim).collect();
            match parts.as_slice() {
                ["bearer", token, ..] => Some(token.to_string()),
                _ => None,
            }
        });
    let token = proto_token
        .or(q.token)
        .ok_or_else(|| ApiError::unauthorized("missing token"))?;
    let user_id = user_for_token(&state, &token)
        .ok_or_else(|| ApiError::unauthorized("invalid token"))?;

    Ok(upgrade
        .protocols(["bearer"])
        .on_upgrade(move |socket| handle_socket(state, user_id, socket)))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    CallJoin { channel_id: String },
    CallLeave { channel_id: String },
    /// A call participant started/stopped sharing their screen (shown as LIVE).
    CallStreaming { channel_id: String, streaming: bool },
    Signal {
        channel_id: String,
        to: String,
        payload: Value,
        sig: String,
    },
    Ping,
}

async fn handle_socket(state: SharedState, user_id: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (conn_id, mut rx) = state.hub.register(&user_id);

    state.hub.send_to(&user_id, &json!({ "type": "hello", "user_id": user_id }));
    send_presence_snapshot(&state, &user_id, conn_id);

    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
            continue;
        };
        handle_client_msg(&state, &user_id, msg);
    }

    // Disconnected: drop the connection and, if it was the last one, leave calls.
    if state.hub.unregister(&user_id, conn_id) {
        for (channel_id, remaining) in state.hub.leave_all(&user_id) {
            broadcast_leave(&state, &channel_id, &user_id, &remaining);
        }
    }
    writer.abort();
}

fn handle_client_msg(state: &SharedState, user_id: &str, msg: ClientMsg) {
    match msg {
        ClientMsg::Ping => state.hub.send_to(user_id, &json!({ "type": "pong" })),
        ClientMsg::CallJoin { channel_id } => {
            if !is_channel_member(state, &channel_id, user_id) {
                return;
            }
            let roster = state.hub.call_join(&channel_id, user_id);
            let roster_event = json!({
                "type": "call_roster", "channel_id": channel_id, "participants": roster,
            });
            state.hub.send_many(&roster, &roster_event);
            let others: Vec<String> =
                roster.iter().filter(|u| *u != user_id).cloned().collect();
            state.hub.send_many(
                &others,
                &json!({ "type": "call_peer_joined", "channel_id": channel_id, "user_id": user_id }),
            );
            broadcast_presence(state, &channel_id);
        }
        ClientMsg::CallLeave { channel_id } => {
            if let Some(remaining) = state.hub.call_leave(&channel_id, user_id) {
                broadcast_leave(state, &channel_id, user_id, &remaining);
            }
        }
        ClientMsg::CallStreaming { channel_id, streaming } => {
            if state.hub.set_streaming(&channel_id, user_id, streaming) {
                broadcast_presence(state, &channel_id);
            }
        }
        ClientMsg::Signal { channel_id, to, payload, sig } => {
            // Relay only between members of the same space who are both in the
            // call. The payload is opaque; clients verify `sig` end-to-end.
            if !is_channel_member(state, &channel_id, user_id)
                || !is_channel_member(state, &channel_id, &to)
                || !state.hub.in_call(&channel_id, user_id)
                || !state.hub.in_call(&channel_id, &to)
            {
                return;
            }
            state.hub.send_to(
                &to,
                &json!({
                    "type": "signal", "channel_id": channel_id,
                    "from": user_id, "payload": payload, "sig": sig,
                }),
            );
        }
    }
}

fn broadcast_leave(state: &SharedState, channel_id: &str, user_id: &str, remaining: &[String]) {
    state.hub.send_many(
        remaining,
        &json!({ "type": "call_peer_left", "channel_id": channel_id, "user_id": user_id }),
    );
    state.hub.send_many(
        remaining,
        &json!({ "type": "call_roster", "channel_id": channel_id, "participants": remaining }),
    );
    broadcast_presence(state, channel_id);
}

/// Who's in a channel's call, for every member of its space — so everyone can
/// see who is talking where before joining (Discord's voice channel list).
fn presence_event(state: &SharedState, channel_id: &str) -> Option<(Value, Vec<String>)> {
    let (space_id, members) = state
        .db
        .with(|c| {
            let Some(space_id) = channel_space(c, channel_id)? else {
                return Ok(None);
            };
            let members = member_ids(c, &space_id)?;
            Ok(Some((space_id, members)))
        })
        .ok()
        .flatten()?;
    let (participants, streaming) = state.hub.presence(channel_id);
    let event = json!({
        "type": "call_presence", "space_id": space_id, "channel_id": channel_id,
        "participants": participants, "streaming": streaming,
    });
    Some((event, members))
}

fn broadcast_presence(state: &SharedState, channel_id: &str) {
    if let Some((event, members)) = presence_event(state, channel_id) {
        state.hub.send_many(&members, &event);
    }
}

/// On connect: the calls currently running in the user's spaces, to the new
/// connection only (the user's other connections are already up to date).
fn send_presence_snapshot(state: &SharedState, user_id: &str, conn_id: u64) {
    for channel_id in state.hub.active_calls() {
        if let Some((event, members)) = presence_event(state, &channel_id) {
            if members.iter().any(|m| m == user_id) {
                state.hub.send_to_conn(user_id, conn_id, &event);
            }
        }
    }
}

fn is_channel_member(state: &SharedState, channel_id: &str, user_id: &str) -> bool {
    state
        .db
        .with(|c| {
            let Some(space_id) = channel_space(c, channel_id)? else {
                return Ok(false);
            };
            Ok(space_role(c, &space_id, user_id)?.is_some())
        })
        .unwrap_or(false)
}
