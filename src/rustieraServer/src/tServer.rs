use crate::auth;
use crate::hysteria::{H3Response, HysController, HysDriver, HysEvent, ProxyEvent};
use crate::proxy::ProxyManager;
use env_logger;
use futures::stream::StreamExt;
use log::{debug, error, info, warn};
use quiche::h3;
use std::collections::BTreeMap;
use bytes::Bytes;
use tokio::runtime::Handle;
use tokio::sync::mpsc::Sender;
use tokio_quiche::listen;
use tokio_quiche::metrics::DefaultMetrics;
use tokio_quiche::quic::SimpleConnectionIdGenerator;
use tokio_quiche::settings::ConnectionParams;
use libRustiera::proto::{HysteriaTCPResponse, HysteriaTCPResponseStatus};

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
    let mut quic_settings = tokio_quiche::settings::QuicSettings::default();
    quic_settings.keylog_file= Some("/tmp/keylog.txt".to_string());
    let mut listeners = listen(
        [socket],
        ConnectionParams::new_server(
            quic_settings,
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
        let (driver, controller) = HysDriver::new();
        conn.unwrap().start(driver);
        debug!("new connection");
        tokio::spawn(handle_connection(controller, Handle::current()));
    }
}
async fn handle_connection(mut controller: HysController, handle: Handle) {
    let mut proxy_map: BTreeMap<u64, Sender<Bytes>> = BTreeMap::new();
    let mut is_verified: bool = false;
    while let Some(event) = controller.event_receiver_mut().recv().await {
        debug!("event received:{:?}", event);
        //each HysEvent correspond to a connection which should have its states
        //verified,
        match event {
            //handle H3 event locally because ideally all auth event should be one shot
            HysEvent::H3Event(stream_id, h3_event, sender) => {
                if !is_verified {
                    match h3_event {
                        h3::Event::Headers { list, .. } => {
                            let (auth_res, response) = auth::auth(list);
                            is_verified = auth_res;
                            //ignore sending error for now
                            sender
                                .send(H3Response { auth_res, response })
                                .await
                                .unwrap();
                        }
                        h3::Event::Finished => {
                            //self.h3_outbound_map.remove(&stream_id);
                            //self.h3conn = None;
                            info!("h3conn finished stream id: {}", stream_id);
                        }
                        other => {
                            info!("other h3 events:{:?}", other);
                        }
                    }
                } else {
                    warn!("Verified user is sending h3 request again");
                }
                //TODO: maybe use oneshot channel
                drop(sender);
            }
            HysEvent::QuicEvent(stream_id, proxy_event) => {
                match proxy_event {
                    ProxyEvent::Request(url, sender) => {
                        info!("Received request for url: {}", url);
                        let (tx, rx) = tokio::sync::mpsc::channel(65535);
                        proxy_map.insert(stream_id, tx);
                        let proxy_response = HysteriaTCPResponse::new(
                            HysteriaTCPResponseStatus::Ok,
                            "Hello World!",
                            "padding",
                        ).into_bytes();
                        sender
                            .send(Bytes::from(proxy_response)).await.unwrap();
                        let mut proxy_manager = ProxyManager::new(url, sender.clone(), rx );
                        handle.spawn(async move {
                            proxy_manager.start().await;
                        });
                    }
                    ProxyEvent::Payload(payload) => {
                        info!("Received proxy payload for stream id: {}", stream_id);
                        if proxy_map.contains_key(&stream_id) {
                            proxy_map
                                .get_mut(&stream_id)
                                .unwrap()
                                .send(payload)
                                .await
                                .unwrap();
                        }else{
                            error!("stream id: {} not registered with a target", stream_id);
                        }
                    }
                }
            }
        }
    }
}
