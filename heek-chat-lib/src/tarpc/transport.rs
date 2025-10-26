use std::pin::Pin;

use futures::{Sink, Stream, task::*};
use tarpc::transport::channel::ChannelError;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct MPSCTransport<Item, SinkItem> {
    rx: mpsc::UnboundedReceiver<Item>,
    tx: mpsc::UnboundedSender<SinkItem>,
}

impl<Item, SinkItem> MPSCTransport<Item, SinkItem> {
    pub fn new(rx: mpsc::UnboundedReceiver<Item>, tx: mpsc::UnboundedSender<SinkItem>) -> Self {
        Self { rx, tx }
    }
}

impl<Item, SinkItem> Stream for MPSCTransport<Item, SinkItem> {
    type Item = Result<Item, ChannelError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Item, ChannelError>>> {
        self.rx
            .poll_recv(cx)
            .map(|option| option.map(Ok))
            .map_err(ChannelError::Receive)
    }
}

const CLOSED_MESSAGE: &str = "the channel is closed and cannot accept new items for sending";

impl<Item, SinkItem> Sink<SinkItem> for MPSCTransport<Item, SinkItem> {
    type Error = ChannelError;

    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(if self.tx.is_closed() {
            Err(ChannelError::Ready(CLOSED_MESSAGE.into()))
        } else {
            Ok(())
        })
    }

    fn start_send(self: Pin<&mut Self>, item: SinkItem) -> Result<(), Self::Error> {
        self.tx
            .send(item)
            .map_err(|_| ChannelError::Send(CLOSED_MESSAGE.into()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // UnboundedSender requires no flushing.
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // UnboundedSender can't initiate closure.
        Poll::Ready(Ok(()))
    }
}
