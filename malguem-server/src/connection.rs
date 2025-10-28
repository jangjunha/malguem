use futures::{SinkExt, Stream, StreamExt};
use malguem_lib::{
    ChatServiceRequest, ChatServiceResponse, Event, MultiplexedMessage,
    tarpc::transport::MPSCTransport,
};
use std::sync::Arc;
use tarpc::{ClientMessage, Response};
use tokio::{
    net::{TcpListener, ToSocketAddrs},
    sync::{
        mpsc::{UnboundedSender, unbounded_channel},
        oneshot,
    },
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

pub struct Connection {
    pub rpc: MPSCTransport<ClientMessage<ChatServiceRequest>, Response<ChatServiceResponse>>,
    pub event_tx: UnboundedSender<Event>,
    pub disconnect_rx: oneshot::Receiver<()>,
}

pub async fn listen<A: ToSocketAddrs>(
    addr: A,
) -> Result<impl Stream<Item = Result<Connection, std::io::Error>>, std::io::Error> {
    let listener = TcpListener::bind(addr).await?;

    Ok(async_stream::stream! {
        loop {
            match listener.accept().await {
                Ok((tcp_stream, _addr)) => {
                    match accept_async(tcp_stream).await {
                        Ok(ws_stream) => {
                            let (ws_write, ws_read) = ws_stream.split();

                            // Create channels for application-level multiplexing
                            let (event_tx, mut event_rx) = unbounded_channel::<Event>();
                            let (rpc_tx, rpc_rx) = unbounded_channel::<ClientMessage<_>>();
                            let (outgoing_tx, outgoing_rx) = unbounded_channel::<Response<_>>();

                            // Create oneshot channel to signal connection closure
                            let (disconnect_tx, disconnect_rx) = oneshot::channel::<()>();
                            let disconnect_tx = Arc::new(tokio::sync::Mutex::new(Some(disconnect_tx)));

                            // Spawn task to handle incoming messages (demultiplex)
                            tokio::spawn({
                                let disconnect_tx = disconnect_tx.clone();
                                let mut ws_read = ws_read;
                                let rpc_tx = rpc_tx.clone();
                                async move {
                                    while let Some(msg_result) = ws_read.next().await {
                                        match msg_result {
                                            Ok(Message::Binary(data)) => {
                                                match serde_cbor::from_slice::<MultiplexedMessage<ClientMessage<_>>>(&data) {
                                                    Ok(MultiplexedMessage::Rpc(rpc)) => {
                                                        if rpc_tx.send(rpc).is_err() {
                                                            tracing::error!("RPC receiver dropped");
                                                            break;
                                                        }
                                                    }
                                                    Ok(MultiplexedMessage::Event(_event)) => {
                                                        // Clients shouldn't send events, ignore
                                                        tracing::warn!("Client sent event, ignoring");
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Failed to decode message: {}", e);
                                                    }
                                                }
                                            }
                                            Ok(Message::Close(_)) => break,
                                            Ok(_) => {}
                                            Err(e) => {
                                                tracing::error!("WebSocket error: {}", e);
                                                break;
                                            }
                                        }
                                    }

                                    // Signal disconnection when incoming task exits
                                    if let Some(tx) = disconnect_tx.lock().await.take() {
                                        let _ = tx.send(());
                                    }
                                }
                            });

                            // Spawn task to handle outgoing messages (multiplex RPC responses and events)
                            tokio::spawn({
                                let disconnect_tx = disconnect_tx.clone();
                                let mut ws_write = ws_write;
                                let mut outgoing_rx = outgoing_rx;
                                async move {
                                    loop {
                                        tokio::select! {
                                            Some(rpc_response) = outgoing_rx.recv() => {
                                                let multiplexed = MultiplexedMessage::Rpc(rpc_response);
                                                match serde_cbor::to_vec(&multiplexed) {
                                                    Ok(data) => {
                                                        if let Err(e) = ws_write.send(Message::Binary(data.into())).await {
                                                            tracing::error!("Failed to send RPC response: {}", e);
                                                            break;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Failed to encode RPC response: {}", e);
                                                    }
                                                }
                                            }
                                            Some(event) = event_rx.recv() => {
                                                let multiplexed: MultiplexedMessage<Response<ChatServiceResponse>> = MultiplexedMessage::Event(event);
                                                match serde_cbor::to_vec(&multiplexed) {
                                                    Ok(data) => {
                                                        if let Err(e) = ws_write.send(Message::Binary(data.into())).await {
                                                            tracing::error!("Failed to send event: {}", e);
                                                            break;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Failed to encode event: {}", e);
                                                    }
                                                }
                                            }
                                            else => break,
                                        }
                                    }

                                    // Signal disconnection when outgoing task exits
                                    if let Some(tx) = disconnect_tx.lock().await.take() {
                                        let _ = tx.send(());
                                    }
                                }
                            });

                            let rpc_transport = MPSCTransport::new(rpc_rx, outgoing_tx);
                            let connection = Connection {
                                rpc: rpc_transport,
                                event_tx,
                                disconnect_rx,
                            };
                            yield Ok::<_, std::io::Error>(connection);
                        }
                        Err(e) => {
                            tracing::error!("WebSocket handshake failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to accept connection: {}", e);
                }
            }
        }
    })
}
