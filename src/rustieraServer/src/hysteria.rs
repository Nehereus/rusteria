use crate::stream::{ReceivedH3Stream, StreamReady, WaitForH3Stream, WaitForStream};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use futures::channel::mpsc::Sender;
use futures::stream::FuturesUnordered;
use log::{debug, error, info, trace, warn};
use std::collections::BTreeMap;
use std::io;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::{runtime, select};
use tokio_quiche::buf_factory::BufFactory;
use tokio_quiche::buffer_pool::PooledBuf;
use tokio_quiche::http3::driver::OutboundFrame;
use tokio_quiche::quic::{HandshakeInfo, QuicheConnection};
use tokio_quiche::{ApplicationOverQuic, QuicResult};

struct TCPProxyManager {
    //in case needs of reconnection
    connection_handle: Option<JoinHandle<Result<TcpStream, io::Error>>>,
    stream: Option<TcpStream>,
    client_outbound_buffer: BytesMut,
    server_outbound_buffer: BytesMut,
    client_outbound_handle: Option<tokio::task::JoinHandle<()>>,
    server_outbound_handle: Option<tokio::task::JoinHandle<()>>,
    runtime: Runtime,
}
impl TCPProxyManager {
    pub fn new(server_addr: String) -> Self {
        let connection_handle = tokio::spawn(TcpStream::connect(server_addr.clone()));
        Self {
            connection_handle: Some(connection_handle),
            stream: None,
            client_outbound_buffer: BytesMut::with_capacity(65535),
            server_outbound_buffer: BytesMut::with_capacity(65535),
            client_outbound_handle: None,
            server_outbound_handle: None,
            runtime: runtime::Builder::new_current_thread().build().unwrap(),
        }
    }
}
#[derive(Debug)]
pub struct H3Response {
    pub(crate) auth_res: bool,
    pub(crate) response: Vec<OutboundFrame>,
}
#[derive(Debug)]
pub enum HysEvent {
    H3Event(u64, quiche::h3::Event, mpsc::Sender<H3Response>),
    QuicEvent(u64, Bytes, Sender<Bytes>),
}

pub struct HysController {
    event_receiver: Option<mpsc::UnboundedReceiver<HysEvent>>,
}
impl HysController {
    //trying to understand why the event receiver is optional
    pub fn event_receiver_mut(&mut self) -> &mut mpsc::UnboundedReceiver<HysEvent> {
        self.event_receiver
            .as_mut()
            .expect("No event receiver in this instance")
    }
}

pub struct HysDriver {
    waiting_streams: FuturesUnordered<WaitForStream>,
    buffer: PooledBuf,
    is_verified: bool,
    h3conn: Option<quiche::h3::Connection>,
    h3config: quiche::h3::Config,
    event_sender: mpsc::UnboundedSender<HysEvent>,
    h3_context_map: BTreeMap<u64, H3Context>,
    //TODO add QUIC context
    tcp_proxy_map: BTreeMap<u64, TCPProxyManager>,
}
struct H3Context {
    receiver: Option< tokio::sync::mpsc::Receiver<H3Response>>,
    queued_frames: Vec<H3Response>,
}
impl HysDriver {
    pub fn new() -> (Self, HysController) {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        (
            Self {
                waiting_streams: FuturesUnordered::new(),
                buffer: BufFactory::get_max_buf(),
                is_verified: false,
                h3conn: None,
                h3config: quiche::h3::Config::new().unwrap(),
                event_sender,
                h3_context_map: BTreeMap::new(),
                tcp_proxy_map: BTreeMap::new(),
            },
            HysController {
                event_receiver: Some(event_receiver),
            },
        )
    }
    fn h3conn_as_mut(&mut self) -> &mut quiche::h3::Connection {
        self.h3conn.as_mut().unwrap()
    }
    fn upstream_ready(
        &mut self,
        qconn: &mut QuicheConnection,
        ready: StreamReady,
    ) -> Result<(), quiche::h3::Error> {
        match ready {
            StreamReady::H3Stream(r) => self.h3_ready(qconn, r),
            StreamReady::QuicStream(r) => Ok(()), //TODO
        }
    }
    fn h3_ready(
        &mut self,
        qconn: &mut QuicheConnection,
        h3_ready: ReceivedH3Stream,
    ) -> Result<(), quiche::h3::Error> {
        let ReceivedH3Stream {
            stream_id,
            chan,
            response,
        } = h3_ready;
        match self.h3_context_map.get_mut(&stream_id) {
            None => Ok(()),
            Some(stream) => {
                // Get the response data before processing
                if let Some(response) = response {
                    stream.queued_frames.push(response);
                }
                stream.receiver = None;
                Ok(())
            }
        }
    }
    fn process_h3_response(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: u64,
        responses: &H3Response,
    ) {
        //TODO
    }
}

#[cfg(not_compile)]
fn handle_h3_write(&mut self, qconn: &mut quiche::Connection, stream_id: u64) -> QuicResult<()> {
    if self.h3_context_map.contains_key(&stream_id) {
        warn!(
            "connection status: {},is established: {}",
            qconn.is_closed(),
            qconn.is_established()
        );

        while let Some(frame) = self.h3_context_map.get_mut(&stream_id).unwrap().last_mut() {
            let res: quiche::h3::Result<()> = match frame {
                OutboundFrame::Headers(h) => {
                    debug!("{} send headers", qconn.trace_id());
                    self.h3conn
                        .as_mut()
                        .unwrap()
                        .send_response(qconn, stream_id, h.as_ref(), false)
                }
                OutboundFrame::Body(body, fin) => {
                    debug!("{} send body on stream: {}", qconn.trace_id(), stream_id);
                    match self
                        .h3conn
                        .as_mut()
                        .unwrap()
                        .send_body(qconn, stream_id, body, *fin)
                    {
                        Ok(n) => {
                            if n == body.len() {
                                Ok(())
                            } else {
                                let (_, new_body) = body.split_at(n);
                                *body = BufFactory::buf_from_slice(new_body);
                                Err(quiche::h3::Error::Done)
                            }
                        }
                        Err(e) => {
                            error!("{} send body error: {}", qconn.trace_id(), e);
                            Err(e)
                        }
                    }
                }
                _ => {
                    warn!("{} unknown frame type: {:?}", qconn.trace_id(), frame);
                    Ok(())
                }
            };
            if res.is_ok() {
                self.h3_context_map.get_mut(&stream_id).unwrap().pop();
            } else {
                break;
            }
        }
    }
    Ok(())
}
#[cfg(not_compile)]
fn handle_quic_read(&mut self, qconn: &mut Connection) -> QuicResult<()> {
    for stream_id in qconn.readable() {
        let mut read_buf: [u8; 65535] = [0; 65535];
        let mut offset = 0;
        while qconn.stream_readable(stream_id) {
            let (read, fin) = qconn.stream_recv(stream_id, &mut read_buf[offset..])?;
            offset += read;
        }
        info!(
            "{} stream parsing TCP request on stream: {}",
            qconn.trace_id(),
            stream_id
        );
        match HysteriaTcpRequest::from_bytes(&read_buf[..offset]) {
            Some(req) => {
                self.tcp_proxy_map
                    .insert(stream_id, TCPProxyManager::new(req.url));
                let resp =
                    HysteriaTCPResponse::new(HysteriaTCPResponseStatus::Ok, "hello", "padding")
                        .into_bytes();

                match qconn.stream_send(stream_id, resp.as_slice(), false) {
                    Ok(n) => {
                        debug!("{} send {} bytes as TCP response", stream_id, n);
                    }
                    Err(e) => {
                        info!("TCP response: {e}");
                    }
                }
            }
            None => {
                if offset == 0 {
                    warn!("Client signifies the end of the stream");
                    match qconn.stream_shutdown(stream_id, Shutdown::Read, 0) {
                        Err(quiche::Error::Done) => {
                            info!(
                                "{} stream {} shutdown gracefully",
                                qconn.trace_id(),
                                stream_id
                            );
                        }
                        Ok(_) => {
                            info!("{} stream {} shutdown ", qconn.trace_id(), stream_id);
                        }
                        Err(e) => {
                            error!(
                                "{} stream {} shutdown error: {}",
                                qconn.trace_id(),
                                stream_id,
                                e
                            );
                        }
                    }
                } else {
                    //COPY data would choose to be blocking
                    match self.tcp_proxy_map.get_mut(&stream_id) {
                        Some(proxy_manager) => {
                            if proxy_manager.stream.is_none()
                                && proxy_manager
                                    .connection_handle
                                    .as_mut()
                                    .unwrap()
                                    .is_finished()
                            {
                                // proxy_manager.stream = proxy_manager.connection_handle.take().unwrap();
                                if let Some(handle) = proxy_manager.connection_handle.take() {
                                    proxy_manager.stream =
                                        Some(proxy_manager.runtime.block_on(handle).unwrap()?);
                                    info!("we get the stream!");
                                }
                            }
                        }
                        None => {
                            error!(
                                "{} TCP proxy not found for stream {}",
                                qconn.trace_id(),
                                stream_id
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

impl ApplicationOverQuic for HysDriver {
    fn on_conn_established(
        &mut self,
        qconn: &mut QuicheConnection,
        handshake_info: &HandshakeInfo,
    ) -> QuicResult<()> {
        info!("{} create h3conn", qconn.trace_id());
        self.h3conn = match quiche::h3::Connection::with_transport(qconn, &self.h3config) {
            Ok(v) => Some(v),

            Err(e) => {
                error!("failed to create HTTP/3 connection: {}", e);
                return Err(Box::new(e));
            }
        };
        Ok(())
    }

    fn should_act(&self) -> bool {
        self.h3conn.is_some() || self.is_verified
    }

    fn buffer(&mut self) -> &mut [u8] {
        trace!("left {} bytes buffer", self.buffer.len());
        &mut self.buffer
    }

    async fn wait_for_data(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        debug!("{} wait for data", qconn.trace_id());

        select! {
            Some(ready) = self.waiting_streams.next() => self.upstream_ready(qconn, ready).unwrap(),
             _ = tokio::time::sleep(Duration::from_millis(5000)) => { println!("outer timeout"); }
    };

        Ok(())
    }

        fn process_reads(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        debug!("{} process reads", qconn.trace_id());
        loop {
            assert!(self.h3conn.is_some());
            if !self.is_verified {
                if let Ok((stream_id, event)) = self.h3conn_as_mut().poll(qconn) {
                    let (tx, rx) = mpsc::channel(65535);
                    self.h3_context_map.insert(
                        stream_id,
                        H3Context {
                            receiver: Some(rx),
                            queued_frames: vec![],
                        },
                    );
                    self.waiting_streams.push(WaitForStream::H3Stream(
                        WaitForH3Stream{
                            stream_id,
                            chan: self.h3_context_map.get_mut(&stream_id).unwrap().receiver.take(),
                        }));

                    self.event_sender
                        .send(HysEvent::H3Event(stream_id, event, tx))
                        .expect("sending failed");


                } else {
                    warn!("{} h3conn poll failed", qconn.trace_id());
                }
            }
            if !qconn.is_readable() {
                break;
            }
        }
        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        for stream_id in qconn.writable() {
            debug!("stream {} writable", stream_id);
            if let Some(context) = self.h3_context_map.get_mut(&stream_id) {
                if let Some(receiver) = context.receiver.as_mut() {
                    match receiver.try_recv() {
                        Ok(h3_response) => {
                            warn!("quiche received the message");
                            // Successfully received, add to queued frames
                            context.queued_frames.push(h3_response);
                            // Set receiver to None since it's been consumed
                            context.receiver = None;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => {
                            self.waiting_streams.push(WaitForStream::H3Stream(
                                WaitForH3Stream{
                                    stream_id,
                                    chan: self.h3_context_map.get_mut(&stream_id).unwrap().receiver.take(),
                                }));
                        }
                        other => info!("H3 response error: {:?}", other),
                    }
                }
            } else if !self.is_verified {
                debug!("new unverified stream id: {}", stream_id);
            }
        }
        Ok(())
    }
}
