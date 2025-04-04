use crate::hysteria::H3Response;
use bytes::Bytes;
use log::{debug};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Receiver;

pub struct HysResponse {
    pub bytes: Bytes,
    pub fin: bool,
}

pub enum WaitForStream {
    H3Stream(WaitForH3Stream),
    QuicStream(WaitForQuicStream),
}
pub enum StreamReady {
    H3Stream(ReceivedH3Stream),
    QuicStream(ReceivedQuicStream),
}
pub struct WaitForH3Stream {
    pub(crate) stream_id: u64,
    pub(crate) chan: Option<Receiver<H3Response>>,
}
pub struct WaitForQuicStream {
    pub(crate) stream_id: u64,
    pub(crate) chan: Option<Receiver<HysResponse>>,
}

pub struct ReceivedH3Stream {
    pub stream_id: u64,
    pub chan: Receiver<H3Response>,
    pub response: Option<H3Response>,
}

pub struct ReceivedQuicStream {
    pub stream_id: u64,
    pub chan: Receiver<HysResponse>,
    pub response: Option<HysResponse>,
}

impl Future for WaitForH3Stream {
    type Output = ReceivedH3Stream;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        debug!(
            "H3 upstream channel closed? {}",
            self.chan.as_mut().unwrap().is_closed()
        );
        self.chan
            .as_mut()
            .unwrap()
            .poll_recv(cx)
            .map(|data| ReceivedH3Stream {
                stream_id: self.stream_id,
                chan: self.chan.take().unwrap(),
                response: data,
            })
    }
}

impl Future for WaitForQuicStream {
    type Output = ReceivedQuicStream;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        debug!(
            "QUIC channel closed? {}",
            self.chan.as_mut().unwrap().is_closed()
        );
        self.chan
            .as_mut()
            .unwrap()
            .poll_recv(cx)
            .map(|data| ReceivedQuicStream {
                stream_id: self.stream_id,
                chan: self.chan.take().unwrap(),
                response: data,
            })
    }
}

impl Future for WaitForStream {
    type Output = StreamReady;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            WaitForStream::H3Stream(d) => Pin::new(d).poll(cx).map(StreamReady::H3Stream),
            WaitForStream::QuicStream(d) => Pin::new(d).poll(cx).map(StreamReady::QuicStream),
        }
    }
}
