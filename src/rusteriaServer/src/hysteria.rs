use crate::stream::{
    HysResponse, ReceivedH3Stream, ReceivedQuicStream, StreamReady, WaitForH3Stream,
    WaitForQuicStream, WaitForStream,
};
use bytes::{ Bytes, BytesMut};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use lib_rusteria::proto::HysteriaTcpRequest;
use log::{debug, error, info, trace, warn};
use quiche::{Connection, Shutdown};
use std::collections::BTreeMap;
use std::ptr::read;
use tokio::select;
use tokio::sync::mpsc;
use tokio_quiche::buf_factory::BufFactory;
use tokio_quiche::buffer_pool::PooledBuf;
use tokio_quiche::http3::driver::OutboundFrame;
use tokio_quiche::quic::{HandshakeInfo, QuicheConnection};
use tokio_quiche::{ApplicationOverQuic, QuicResult};
use tokio_quiche::metrics::Metrics;

const MAX_BUF_SIZE: usize = 65535 * 5; //64kb*5

#[derive(Debug)]
pub struct H3Response {
    pub(crate) auth_res: bool,
    pub(crate) response: Vec<OutboundFrame>,
}
#[derive(Debug)]
pub enum HysEvent {
    H3Event(u64, quiche::h3::Event, mpsc::Sender<H3Response>),
    QuicEvent(u64, ProxyEvent),
}
#[derive(Debug)]
pub enum ProxyEvent {
    //url
    Request(String, mpsc::Sender<HysResponse>),
    Payload(Bytes),
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
    quic_context_map: BTreeMap<u64, QuicContext>,
}
struct H3Context {
    queued_frames: Vec<H3Response>,
}
struct QuicContext {
    //TODO undetermined implementation
    queued_bytes: BytesMut,
    fin: bool,
    read_fin: bool,
}
impl HysDriver {
    pub fn new() -> (Self, HysController) {
        let mut h3config = quiche::h3::Config::new().unwrap();
        h3config.set_qpack_max_table_capacity(64);
        h3config.set_qpack_blocked_streams(64);

        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        (
            Self {
                waiting_streams: FuturesUnordered::new(),
                buffer: BufFactory::get_max_buf(),
                is_verified: false,
                h3conn: None,
                h3config,
                event_sender,
                h3_context_map: BTreeMap::new(),
                quic_context_map: BTreeMap::new(),
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
            StreamReady::QuicStream(r) => self.quic_ready(qconn, r),
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
                    //if the verification attempt fails, rearm the receiver
                    // if !response.auth_res{
                    //         self.waiting_streams
                    //             .push(WaitForStream::H3Stream(WaitForH3Stream {
                    //                 stream_id,
                    //                 chan: Some(chan),
                    //             }));
                    // }
                    stream.queued_frames.push(response);
                }
                Ok(())
            }
        }
    }
    fn quic_ready(
        &mut self,
        qconn: &mut QuicheConnection,
        quic_ready: ReceivedQuicStream,
    ) -> Result<(), quiche::h3::Error> {
        let ReceivedQuicStream {
            stream_id,
            chan,
            response,
        } = quic_ready;
        match self.quic_context_map.get_mut(&stream_id) {
            None => Ok(()),
            Some(stream) => {
                // Get the response data before processing
                if let Some(response) = response {
                    //rearm the waiting_streams if not finished and
                    if !(chan.is_closed() && response.fin) {
                        self.waiting_streams
                            .push(WaitForStream::QuicStream(WaitForQuicStream {
                                stream_id,
                                chan: Some(chan),
                            }));
                    }
                    info!(
                        "{} quic stream {} read: {}",
                        qconn.trace_id(),
                        stream_id,
                        response.bytes.len()
                    );
                    stream.queued_bytes.extend_from_slice(&response.bytes);
                    stream.fin |= response.fin;
                }
                Ok(())
            }
        }
    }

    //process the queued frames and return unprocessed ones if any
    fn handle_h3_response(
        &mut self,
        qconn: &mut QuicheConnection,
        stream_id: u64,
        responses: &H3Response,
    ) -> Result<Vec<OutboundFrame>, quiche::h3::Error> {
        self.is_verified |= responses.auth_res;
        let mut sending_results: Vec<Option<OutboundFrame>> = vec![];
        for frame in responses.response.iter() {
            sending_results.push(match frame {
                OutboundFrame::Headers(h) => {

                    if self
                        .h3conn_as_mut()
                        .send_response(qconn, stream_id, h.as_ref(),responses.auth_res)
                        .is_err()
                    {
                        Some(OutboundFrame::Headers((*h.clone()).to_owned()))
                    } else {
                        debug!("{} send headers", qconn.trace_id());
                        None
                    }
                }

                OutboundFrame::Body(body, fin) => {
                    debug!("{} send body on stream: {}", qconn.trace_id(), stream_id);
                    match self
                        .h3conn_as_mut()
                        .send_body(qconn, stream_id, &body, *fin)
                    {
                        Ok(n) => {
                            if n == body.len() {
                                None
                            } else {
                                let (_, new_body) = body.split_at(n);
                                //re-push the remaining body back to the queue
                                //this works because we will .pop to traverse the queue,
                                //instead of referencing part of the queue,
                                //we extract the frame out of the queue then reference it
                                Some(OutboundFrame::Body(
                                    BufFactory::buf_from_slice(new_body),
                                    *fin,
                                ))
                            }
                        }
                        Err(e) => {
                            error!("{} send body error: {}", qconn.trace_id(), e);
                            Some(OutboundFrame::Body(BufFactory::buf_from_slice(body), *fin))
                        }
                    }
                }
                _ => {
                    warn!("{} unknown frame type: {:?}", qconn.trace_id(), frame);
                    None
                }
            });
        }
        //TODO collect the sending errors
        Ok(vec![])
    }

    // #[cfg(not_compile)]
    // should send one of the HysEvent to the receiver
    fn handle_quic_request(&mut self, qconn: &mut Connection) -> QuicResult<()> {
        for stream_id in qconn.readable() {
            if let Some(context) = self.quic_context_map.get_mut(&stream_id) {
                if !context.read_fin && context.fin {
                    //discard all remaining info in the channel
                    if let Err(e) = qconn.stream_shutdown(stream_id, Shutdown::Read, 0) {
                        trace!(
                                    "{} stream {} shutdown error: {}",
                                    qconn.trace_id(),
                                    stream_id,
                                    e
                                );
                    }
                    continue;
                }
            }
            let mut offset = 0;
            while qconn.stream_readable(stream_id) {
                //Fin signal will be handled at the end
                let (read,_) = qconn.stream_recv(stream_id, &mut self.buffer)?;
                debug!(
                    "{} quic stream {} read: {}",
                    qconn.trace_id(),
                    stream_id,
                    read
                );
                offset += read;
            }
            if offset == 0 {
                info!(
                    "Client signifies the end of the stream, stream id: {}",
                    stream_id
                );
                //this is highly coupling, which handles the event locally
                //but since the only use of a 0 offset receive event by definition
                //is the end of the stream, we just handle it here
                let shutdown_result = qconn.stream_shutdown(stream_id, Shutdown::Read, 0);
                //TODO refactor. coding style is awakward
                match shutdown_result {
                    Ok(_) | Err(quiche::Error::Done) => {
                        info!(
                            "{} stream {} was shutdown gracefully",
                            qconn.trace_id(),
                            stream_id
                        );
                    }
                    Err(e) => {
                        warn!(
                            "{} stream {} shutdown error: {}",
                            qconn.trace_id(),
                            stream_id,
                            e
                        );
                    }
                }
            } else {
                let (tx, rx) = mpsc::channel(MAX_BUF_SIZE);
                let mut events: Vec<HysEvent> = Vec::new();
                let mut read_data = std::mem::replace(&mut self.buffer, BufFactory::get_max_buf());
                read_data.truncate(offset);
                //determine if this is a new proxy request or payload of an existing request
                // if let Some(req) =HysteriaTcpRequest::from_bytes(&read_buf[..offset]) {
                //         if self.quic_context_map.get_mut(&stream_id).is_none() {
                if self.quic_context_map.get_mut(&stream_id).is_none() {
                    if let Some(req) = HysteriaTcpRequest::from_bytes(&**read_data) {
                        let _ = events.push(HysEvent::QuicEvent(
                            stream_id,
                            ProxyEvent::Request(req.url, tx),
                        ));
                        self.quic_context_map.insert(
                            stream_id,
                            QuicContext {
                                queued_bytes: BytesMut::with_capacity(MAX_BUF_SIZE),
                                fin: false,
                                read_fin: false,
                            },
                        );
                       let (_ ,temp) = read_data.split_at(req.offset);
                        read_data = BufFactory::buf_from_slice(temp);
                    } else {
                        error!(
                            "client is sending invalid initial proxy request on stream {}",
                            stream_id
                        );
                    }
                 }
             if !read_data.is_empty(){
                //have to clone
                let inbound_bytes = Bytes::copy_from_slice(&read_data);
                let _ = events.push(HysEvent::QuicEvent(
                    stream_id,
                    ProxyEvent::Payload(inbound_bytes),
                ));
                }

                if(!events.is_empty()){
                    self.waiting_streams
                        .push(WaitForStream::QuicStream(WaitForQuicStream {
                            stream_id,
                            chan: Some(rx),
                        }));
                }
                for event in events {
                    trace!(
                    "{} sending event to the handler: {:?}",
                    qconn.trace_id(),
                    event
                );
                    //TODO: investigate why client ignores the fin signal
                    if let Err(e) = self.event_sender.send(event) {
                        //TODO receiver closed
                        error!("Failed to send event to the handler: {}", e);
                    }
                }
            }
        }
        Ok(())
    }
    fn handle_h3_request(&mut self, qconn: &mut Connection) -> QuicResult<()> {
        match self.h3conn_as_mut().poll(qconn) {
            Ok((stream_id, event)) => {
                let (tx, rx) = mpsc::channel(MAX_BUF_SIZE);

                self.h3_context_map.insert(
                    stream_id,
                    H3Context {
                        queued_frames: vec![],
                    },
                );
                self.waiting_streams
                    .push(WaitForStream::H3Stream(WaitForH3Stream {
                        stream_id,
                        chan: Some(rx),
                    }));

                self.event_sender
                    .send(HysEvent::H3Event(stream_id, event, tx))
                    .expect("sending failed");
                Ok(())
            }
            Err(quiche::h3::Error::Done) => {
                debug!("{} h3 conn done", qconn.trace_id());
                Ok(())
            }
            Err(e) => {
                error!("{} h3 conn error: {}", qconn.trace_id(), e);
                Err(Box::new(e))
            }
        }
    }
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
                //_ = tokio::time::sleep(Duration::from_millis(500)) => { println!("outer timeout"); }
                _ = std::future::pending::<()>() => unreachable!(),
        };

        Ok(())
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        while qconn.is_readable() {
            debug!("{} process reads", qconn.trace_id());
            if !self.is_verified {
                self.handle_h3_request(qconn)?
            } else {
                self.handle_quic_request(qconn)?;
            }
        }
        Ok(())
    }

    fn process_writes(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        for stream_id in qconn.writable() {
            trace!("stream {} writable", stream_id);
            if !self.is_verified {
                if let Some(context) = self.h3_context_map.get_mut(&stream_id) {
                    if !context.queued_frames.is_empty() {
                        debug!(
                            "Writing {} h3 frame(s) to the client",
                            context.queued_frames.len()
                        );
                        let mut responses_to_process = Vec::new();
                        while let Some(response) = context.queued_frames.pop() {
                            responses_to_process.push(response);
                        }
                        for response in responses_to_process {
                            self.handle_h3_response(qconn, stream_id, &response)
                                .expect("TODO: panic message");
                        }
                    }
                } else {
                    //TODO: proper logging
                    trace!("new unverified stream id: {}", stream_id);
                }
            }
                if let Some(context) = self.quic_context_map.get_mut(&stream_id) {
                    if !context.queued_bytes.is_empty() {
                        debug!(
                            "writing len {} bytes quic traffic to the client",
                            context.queued_bytes.len()
                        );
                        trace!(
                            "bytes content in ascii: {}",
                            String::from_utf8_lossy(&context.queued_bytes)
                        );
                        let sent = qconn.stream_send(stream_id, &*context.queued_bytes, false)?;
                        context.queued_bytes = context.queued_bytes.split_off(sent);
                        context.queued_bytes.reserve(65535);
                    }else if context.fin {
                        if let Err(e) = qconn.stream_shutdown(stream_id, Shutdown::Write, 0) {
                            error!(
                                "{} stream {} shutdown error: {}",
                                qconn.trace_id(),
                                stream_id,
                                e
                            );
                        }
                        if let Err(e) = qconn.stream_send(stream_id, b"", true) {
                            error!(
                                "{} stream {} sending fi signal to the client: {}",
                                qconn.trace_id(),
                                stream_id,
                                e
                            );
                        }
                        self.quic_context_map.remove(&stream_id);
                        info!("{} stream {} fin", qconn.trace_id(), stream_id);
                    }
            }
        }
        Ok(())
    }

    fn on_conn_close<M: Metrics>(
        &mut self,
        qconn: &mut QuicheConnection,
        metrics: &M,
        connection_result: &QuicResult<()>,
    ){
        self.process_writes(qconn).unwrap();

        warn!("{}: quic connection closed", qconn.trace_id());
    }
}
