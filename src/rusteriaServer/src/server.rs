use crate::auth;
use crate::hysteria::{H3Response, HysController, HysDriver, HysEvent, ProxyEvent};
use crate::proxy::ProxyManager;
use crate::stream::HysResponse;
use bytes::Bytes;
use futures::stream::StreamExt;
use lib_rusteria::proto::{HysteriaTCPResponse, HysteriaTCPResponseStatus};
use log::{debug, error, info, warn};
use quiche::h3;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;
use tokio_quiche::listen;
use tokio_quiche::metrics::DefaultMetrics;
use tokio_quiche::quic::SimpleConnectionIdGenerator;
use tokio_quiche::settings::ConnectionParams;
use crate::commandline::parse_args;

#[tokio::main]
pub async fn server() {
    //console_subscriber::init();
    let config = parse_args();
    let socket = tokio::net::UdpSocket::bind(&config.addr).await.unwrap();
    info!("listening on {}", config.addr);
    let mut quic_settings = tokio_quiche::settings::QuicSettings::default();
    quic_settings.keylog_file = Some("/tmp/keylog.txt".to_string());
    let mut listeners = listen(
        [socket],
        ConnectionParams::new_server(
            quic_settings,
            tokio_quiche::settings::TlsCertificatePaths {
                cert: &config.cert_path,
                private_key: &config.key_path,
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
        let auth_token = config.auth_token.clone();
        tokio::spawn(handle_connection(controller, Handle::current(), auth_token));
    }
}
async fn handle_connection(mut controller: HysController, handle: Handle, auth_token: String) {
    let proxy_map: Arc<Mutex<BTreeMap<u64, Sender<Bytes>>>> = Arc::new(Mutex::new(BTreeMap::new()));
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
                            let (auth_res, response) = auth::auth(list, &auth_token);
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
                        {
                            let mut locked_map = proxy_map.lock().await;
                            locked_map.insert(stream_id, tx);
                        }

                        let proxy_response = HysteriaTCPResponse::new(
                            HysteriaTCPResponseStatus::Ok,
                            "Hello World!",
                            "padding",
                        )
                        .into_bytes();
                        sender
                            .send(HysResponse {
                                bytes: Bytes::from(proxy_response),
                                fin: false,
                            })
                            .await
                            .unwrap();
                        let mut proxy_manager = ProxyManager::new(url, sender.clone(), rx);

                        let proxy_map_ref = Arc::clone(&proxy_map);
                        handle.spawn(async move {
                            //keep a local copy of the stream id
                            proxy_manager.start().await;
                            //cleanup
                            let mut map = proxy_map_ref.lock().await;
                            map.remove(&stream_id);
                        });
                    }
                    ProxyEvent::Payload(payload) => {
                        info!("Received proxy payload for stream id: {}", stream_id);
                        let mut map = proxy_map.lock().await;
                        if let Some(sender) = map.get_mut(&stream_id) {
                            if let Err(e) = sender.send(payload).await {
                                error!("Failed to send payload to the proxy unit: {}", e);
                            }
                        } else {
                            error!("stream id: {} not registered with a target", stream_id);
                        }
                    }
                };
            }
        }
    }
}
