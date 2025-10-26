use futures::{SinkExt, StreamExt};
use heek_chat_lib::{
    ChatServiceClient, Event, MultiplexedMessage, tarpc::transport::MPSCTransport,
};
use tarpc::{ClientMessage, Response};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

pub struct Connection {
    pub rpc: ChatServiceClient,
    pub event_rx: UnboundedReceiver<Event>,
}

impl Connection {
    pub async fn establish<R>(request: R) -> Result<Self, String>
    where
        R: IntoClientRequest + Unpin,
    {
        let (ws_stream, _) = connect_async(request)
            .await
            .map_err(|e| format!("WebSocket connection failed: {}", e))?;

        let (ws_write, ws_read) = ws_stream.split();

        // Create channels for application-level multiplexing
        let (outgoing_tx, outgoing_rx) = unbounded_channel::<ClientMessage<_>>();
        let (rpc_tx, rpc_rx) = unbounded_channel::<Response<_>>();
        let (event_tx, event_rx) = unbounded_channel::<Event>();

        // Spawn task to handle incoming messages (demultiplex)
        tokio::spawn({
            let mut ws_read = ws_read;
            let rpc_tx = rpc_tx.clone();
            async move {
                while let Some(msg_result) = ws_read.next().await {
                    match msg_result {
                        Ok(Message::Binary(data)) => {
                            // Decode CBOR message
                            match serde_cbor::from_slice::<MultiplexedMessage<Response<_>>>(&data) {
                                Ok(MultiplexedMessage::Rpc(rpc)) => {
                                    if rpc_tx.send(rpc).is_err() {
                                        tracing::error!("RPC receiver dropped");
                                        break;
                                    }
                                }
                                Ok(MultiplexedMessage::Event(event)) => {
                                    if event_tx.send(event).is_err() {
                                        tracing::warn!("Event receiver dropped");
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to decode message: {}", e);
                                }
                            }
                        }
                        Ok(Message::Close(_)) => break,
                        Ok(_) => {} // Ignore other message types
                        Err(e) => {
                            tracing::error!("WebSocket error: {}", e);
                            break;
                        }
                    }
                }
            }
        });

        // Spawn task to handle outgoing messages (multiplex)
        tokio::spawn({
            let mut ws_write = ws_write;
            let mut outgoing_rx = outgoing_rx;
            async move {
                while let Some(rpc_msg) = outgoing_rx.recv().await {
                    let multiplexed = MultiplexedMessage::Rpc(rpc_msg);
                    match serde_cbor::to_vec(&multiplexed) {
                        Ok(data) => {
                            if let Err(e) = ws_write.send(Message::Binary(data.into())).await {
                                tracing::error!("Failed to send message: {}", e);
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to encode message: {}", e);
                        }
                    }
                }
            }
        });

        let rpc_transport = MPSCTransport::new(rpc_rx, outgoing_tx);
        let rpc = ChatServiceClient::new(tarpc::client::Config::default(), rpc_transport).spawn();

        Ok(Self { rpc, event_rx })
    }
}
