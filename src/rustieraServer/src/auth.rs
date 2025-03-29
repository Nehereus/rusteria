use crate::structs::{Client, PartialResponse};
use libRustiera::proto::*;
use log::{error, info, warn};
use quiche::h3::Header;
use quiche::h3::NameValue;
use tokio_quiche::buf_factory::{BufFactory, PooledBuf};
use tokio_quiche::buffer_pool::Pooled;
use tokio_quiche::http3::driver::OutboundFrame;

//TODO: auth should always return a response regardless of the auth result;
// in case of success auth, it should return a success AuthResponse
// else return masq
pub fn auth(headers: Vec<Header>) -> (bool, Vec<OutboundFrame>) {
    if let Ok(req) = AuthRequest::from_event_header(headers.as_slice()) {
        if verify(req.auth_token) {
            let auth_resp = AuthResponse {
                status: 233,
                udp_supported: false,
                server_rx: 0,
                rx_auto: true,
                padding: "8964",
            };
            let v = vec![OutboundFrame::Headers(auth_resp.to_headers())];
            (true, v)
        } else {
            (false, masquerade())
        }
    } else {
        (false, masquerade())
    }
}
//TODO implement verification
fn verify(token: &str) -> bool {
    return token.eq("8964");
}

//TODO implement masquerade
fn masquerade() -> Vec<OutboundFrame> {
    let headers = OutboundFrame::Headers(vec![Header::new(b":status", 200.to_string().as_bytes())]);
    let body = OutboundFrame::body(BufFactory::buf_from_slice(b"Hello World!"), true);
    let v = vec![headers, body];
    v
}
