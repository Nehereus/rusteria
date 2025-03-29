use crate::auth;
use futures::future::{err, ok};
use libRustiera::proto::{HysteriaTCPResponse, HysteriaTCPResponseStatus, HysteriaTcpRequest};
use log::{debug, error, info, warn};
use quiche::{Connection, Header, Shutdown};
use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::pin::Pin;
use std::thread::sleep;
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio::task::{spawn, JoinHandle};
use tokio_quiche::http3::driver::{H3Driver, H3Event, OutboundFrame};
use tokio_quiche::quic::{HandshakeInfo, QuicheConnection};
use tokio_quiche::{ApplicationOverQuic, QuicConnection, QuicResult};
use bytes::{Bytes, BytesMut};
use tokio::try_join;
use tokio_quiche::buf_factory::BufFactory;
use tokio::runtime;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

struct TCPProxyManager{
    //in case needs of reconnection
    connection_handle: Option<JoinHandle<Result<TcpStream, io::Error>>>,
    stream:Option<TcpStream>,
    client_outbound_buffer:BytesMut,
    server_outbound_buffer:BytesMut,
    client_outbound_handle:Option<tokio::task::JoinHandle<()>>,
    server_outbound_handle:Option<tokio::task::JoinHandle<()>>,
    runtime:Runtime,
}
impl TCPProxyManager{
    pub fn new(server_addr:String) -> Self {
        let connection_handle= tokio::spawn(TcpStream::connect(server_addr.clone()));
        Self {
            connection_handle:Some(connection_handle),
            stream:None,
            client_outbound_buffer:BytesMut::with_capacity(65535),
            server_outbound_buffer:BytesMut::with_capacity(65535),
            client_outbound_handle:None,
            server_outbound_handle:None,
            runtime:runtime::Builder::new_current_thread().build().unwrap(),
        }
    }

}
pub struct QuicEvent{
  pub stream_id:u64,
  pub  body:Bytes
}
pub enum HysEvent{
   H3Event(u64, quiche::h3::Event,mpsc::Sender<OutboundFrame>),
    QuicEvent(u64,QuicEvent),
}

pub struct HysController {
    event_receiver:Option<mpsc::UnboundedReceiver<HysEvent>>,
}
impl HysController {
    //trying to understand why the event receiver is optional
    pub fn event_receiver_mut(&mut self)->&mut mpsc::UnboundedReceiver<HysEvent>{
        self.event_receiver.as_mut().expect("No event receiver in this instance")
    }
}

pub struct HysDriver {
    is_verified: bool,
    h3conn: Option<quiche::h3::Connection>,
    h3config: quiche::h3::Config,
    event_sender: mpsc::UnboundedSender<HysEvent>,
    h3_outbound_map: BTreeMap<u64, Vec<OutboundFrame>>,
    tcp_proxy_map: BTreeMap<u64, TCPProxyManager>,
}
impl HysDriver {
    fn is_h3_packet(payload: &[u8]) -> bool {
        if let Some(&first_byte) = payload.first() {
            matches!(first_byte, 0x00 | 0x01 | 0x03 | 0x04) // H3 frame types
        } else {
            false
        }
    }

    pub fn new() -> (Self,HysController) {
       let (event_sender,event_receiver)=mpsc::unbounded_channel();
        (Self {
            is_verified: false,
            h3conn: None,
            h3config: quiche::h3::Config::new().unwrap(),
            event_sender,
            h3_outbound_map: BTreeMap::new(),
            tcp_proxy_map: BTreeMap::new(),
        },HysController{
            event_receiver:Some(event_receiver)
        }
        )
    }
    fn h3conn_as_mut(&mut self) -> &mut quiche::h3::Connection {
        self.h3conn.as_mut().unwrap()
    }
    fn handle_h3_request(
        &mut self,
        qconn: &mut Connection,
        stream_id: u64,
        headers: &[quiche::h3::Header],
    ) {
        info!(
            "Got request {:?} on stream id {:?}",
            qconn.trace_id(),
            stream_id
        );

        // We decide the response based on headers alone, so stop reading the
        // request stream so that anybody is ignored and pointless Data events
        // are not generated.
        // qconn.stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
        // 	.unwrap();

        let (auth_res, resp) = auth::auth(headers.to_vec());

        if !self.is_verified {
            self.is_verified = auth_res;
        }

        if (!self.h3_outbound_map.contains_key(&stream_id)) {
            self.h3_outbound_map.insert(stream_id, Vec::new());
        }
        self.h3_outbound_map
            .get_mut(&stream_id)
            .unwrap()
            .extend(resp.into_iter().rev());
    }
    fn handle_h3_write(
        &mut self,
        qconn: &mut quiche::Connection,
        stream_id: u64,
    ) -> QuicResult<()> {
        //TODO: should not remove the current frame when sending failed
        if self.h3_outbound_map.contains_key(&stream_id) {
            warn!("connection status: {},is established: {}", qconn.is_closed(), qconn.is_established());

            while let Some(frame) = self.h3_outbound_map.
                get_mut(&stream_id).unwrap().last_mut() {
                 let res:quiche::h3::Result<()> =  match frame {
                        OutboundFrame::Headers(h) => {
                            debug!("{} send headers", qconn.trace_id());
                            self.h3conn.as_mut().unwrap().send_response(
                                qconn,
                                stream_id,
                                h.as_ref(),
                                false,
                            )
                        }
                        OutboundFrame::Body(body,fin) => {
                            debug!("{} send body on stream: {}", qconn.trace_id(),stream_id);
                           match self.h3conn.as_mut().unwrap().send_body(
                                qconn,
                                stream_id,
                                body,
                                *fin,
                            ){
                               Ok(n) =>{
                                   if n==body.len(){
                                       Ok(())
                                   }else {
                                      let (_,new_body)=body.split_at(n);
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
                            warn!("{} unknown frame type: {:?}", qconn.trace_id(),frame);
                            Ok(())
                        }
                    };
                if res.is_ok() { self.h3_outbound_map.get_mut(&stream_id).unwrap().pop();}
                else{
                    break;
                }

                }
            }
        Ok(())
    }
    fn handle_quic_read(&mut self,qconn: &mut Connection) -> QuicResult<()>{
        for stream_id in qconn.readable() {
            let mut read_buf: [u8; 65535] = [0; 65535];
            let mut offset = 0;
            while qconn.stream_readable(stream_id) {
                let (read, fin) = qconn.stream_recv(stream_id, &mut read_buf[offset..])?;
                offset += read;
            }
            info!("{} stream parsing TCP request on stream: {}", qconn.trace_id(), stream_id);
            match HysteriaTcpRequest::from_bytes(&read_buf[..offset]) {
                Some(req) => {
                    self.tcp_proxy_map.insert(
                        stream_id,
                        TCPProxyManager::new(req.url),
                    );
                    let resp = HysteriaTCPResponse::new(
                        HysteriaTCPResponseStatus::Ok,
                        "hello",
                        "padding",
                    ).into_bytes();

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
                                info!(
                                            "{} stream {} shutdown ",
                                            qconn.trace_id(),
                                            stream_id
                                        );
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
                                if proxy_manager.stream.is_none() && proxy_manager.connection_handle.as_mut().unwrap().is_finished() {
                                     // proxy_manager.stream = proxy_manager.connection_handle.take().unwrap();
                                    if let Some(handle) = proxy_manager.connection_handle.take() {
                                       proxy_manager.stream=Some( proxy_manager.runtime.block_on(handle).unwrap()?);
                                        info!("we get the stream!");
                                    }
                                }
                            }
                            None => {
                                error!("{} TCP proxy not found for stream {}", qconn.trace_id(), stream_id);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl ApplicationOverQuic for HysDriver {
    fn on_conn_established(
        &mut self,
        qconn: &mut QuicheConnection,
        handshake_info: &HandshakeInfo,
    ) -> QuicResult<()> {
        if self.h3conn.is_none() {
            info!("{} create h3conn", qconn.trace_id());
            self.h3conn = match quiche::h3::Connection::with_transport(qconn, &self.h3config) {
                Ok(v) => Some(v),

                Err(e) => {
                    error!("failed to create HTTP/3 connection: {}", e);
                    return Err(Box::new(e));
                }
            };
            Ok(())
        } else {
            error!("{} h3conn already created", qconn.trace_id());
            Err(quiche::h3::Error::Done)?
        }
    }

    fn should_act(&self) -> bool {
        self.h3conn.is_some() || self.is_verified
    }

    fn buffer(&mut self) -> &mut [u8] {
        self.h3driver.buffer()
    }

    fn wait_for_data(
        &mut self,
        qconn: &mut QuicheConnection,
    ) -> impl Future<Output = QuicResult<()>> + Send {
        self.h3driver.wait_for_data(qconn)
    }

    fn process_reads(&mut self, qconn: &mut QuicheConnection) -> QuicResult<()> {
        loop {
                assert!(self.h3conn.is_some());
            if self.is_verified{
               if let Ok((stream_id,event))= self.h3conn_as_mut().poll(qconn) {
                    self.event_sender.send(HysEvent::H3Event(stream_id,event,));
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
            debug!("stream id writable: {}", stream_id);
            if self.h3_outbound_map.contains_key(&stream_id) {
                self.handle_h3_write(qconn, stream_id)?
            } else if !self.is_verified {
                debug!("new unverified stream id: {}", stream_id);
            }
        }
        QuicResult::Ok(())
    }

}
