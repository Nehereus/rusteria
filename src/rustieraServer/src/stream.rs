use crate::hysteria::H3Response;
use bytes::Bytes;
use std::pin::Pin;
use std::task::{Context, Poll};
use log::error;
use tokio::sync::mpsc::Receiver;
use tokio_quiche::datagram_socket::DatagramSocketRecv;

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
    stream_id: u64,
    chan: Option<Receiver<Bytes>>,
}

pub struct ReceivedH3Stream {
    pub stream_id: u64,
    pub chan: Receiver<H3Response>,
    pub response: Option<H3Response>,
}

pub struct ReceivedQuicStream {
    stream_id: u64,
    chan: Receiver<Bytes>,
    response: Option<Bytes>,
}

impl Future for WaitForH3Stream {
    type Output = ReceivedH3Stream;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        error!("{}",self.chan.as_mut().unwrap().is_closed());
        self.chan.as_mut().unwrap().poll_recv(cx).map(|data| {
            ReceivedH3Stream {
                stream_id: self.stream_id,
                chan: self.chan.take().unwrap(),
                response: data,
            }
        })
    }
}

impl Future for WaitForQuicStream {
    type Output = ReceivedQuicStream;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
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
