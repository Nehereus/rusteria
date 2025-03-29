use std::collections::BTreeMap;
use crate::auth::auth;
use crate::hysteria::{HysController, HysDriver, HysEvent};
use env_logger;
use futures::SinkExt;
use futures::stream::StreamExt;
use log::{error, info, warn};
use quiche::{h3, Connection};
use tokio::net::{TcpStream, UdpSocket};
use tokio_quiche::ServerH3Driver;
use tokio_quiche::buf_factory::BufFactory;
use tokio_quiche::http3::driver::{H3Event, IncomingH3Headers, OutboundFrame, ServerH3Event};
use tokio_quiche::http3::settings::Http3Settings;
use tokio_quiche::metrics::DefaultMetrics;
use tokio_quiche::quic::SimpleConnectionIdGenerator;
use tokio_quiche::settings::ConnectionParams;
use tokio_quiche::{ServerH3Controller, listen};
use crate::handler::{handler, stream_handler};
use tokio::runtime::Handle;
use crate::auth;

const HOSTNAME: &str = "0.0.0.0";
const LISTEN_PORT: u16 = 8888;

pub fn main() {
    server();
}

#[tokio::main]
async fn server() {
    env_logger::init();
    let addr: String = format!("{}:{}", HOSTNAME, LISTEN_PORT);
    let socket = tokio::net::UdpSocket::bind(addr).await.unwrap();
    let mut listeners = listen(
        [socket],
        ConnectionParams::new_server(
            //modify quic config here
            tokio_quiche::settings::QuicSettings::default(),
            tokio_quiche::settings::TlsCertificatePaths {
                cert: "/tmp/cert/cert.pem",
                private_key: "/tmp/cert/key.pem",
                kind: tokio_quiche::settings::CertificateKind::X509,
            },
            Default::default(),
        ),
        SimpleConnectionIdGenerator,
        DefaultMetrics,
    )
    .unwrap();
    let accept_stream = &mut listeners[0];
    while let Some(conn) = accept_stream.next().await {
        let(driver,controller)=HysDriver::new();
        conn.unwrap().start(driver);
        tokio::spawn(handle_connection(controller,Handle::current()));
    }
}
async fn handle_connection(mut controller: HysController,handle:Handle) {
    let mut stream_map:BTreeMap<u64,stream_handler>=BTreeMap::new();
    let mut verified:bool=false;
    while let Some(event) = controller.event_receiver_mut().recv().await {
        //each HysEvent correspond to a connection which should have its states
        //verified,
        match event {
            //handle H3 event locally because ideally all auth event should be one shot
            HysEvent::H3Event(stream_id,h3_event,sender) => {
                if !verified{
                    match h3_event {
                        Ok(h3::Event::Headers { list, .. }) => {
                            let (auth_res, resps) = auth::auth(list);
                            verified=auth_res;
                            //ignore sending error for now
                            for resp in resps{sender.send(resp).unwrap()}
                        }
                        Ok(h3::Event::Finished) => {
                            //self.h3_outbound_map.remove(&stream_id);
                            //self.h3conn = None;
                            info!(
                            "h3conn finished stream id: {}",
                            stream_id
                        );
                        }
                        other=>{
                            info!("other h3 events:{}",other);
                        }

                    }
                }else{
                    warn!("Verified user is sending h3 request again");
                }

            }
            HysEvent::QuicEvent(stream_id,q_event) => {

            }
        }

    }
}
