use lib_rusteria::proto::*;
use quiche::h3::Header;
use tokio_quiche::buf_factory::BufFactory;
use tokio_quiche::http3::driver::OutboundFrame;

//TODO: auth should always return a response regardless of the auth result;
// in case of success auth, it should return a success AuthResponse
// else return masq
pub fn auth(headers: Vec<Header>, auth_token: &str) -> (bool, Vec<OutboundFrame>) {
    if let Ok(req) = AuthRequest::from_event_header(headers.as_slice()) {
        if verify(req.auth_token, auth_token) {
            let auth_resp = AuthResponse {
                status: 233,
                udp_supported: false,
                server_rx: 0,
                rx_auto: true,
                padding: "padding",
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
fn verify(token: &str, auth_token: &str) -> bool {
    token.eq(auth_token)
}

//TODO implement masquerade
fn masquerade() -> Vec<OutboundFrame> {
    let headers = OutboundFrame::Headers(vec![Header::new(b":status", 200.to_string().as_bytes())]);
    let body = OutboundFrame::body(BufFactory::buf_from_slice(b"Hello World!"), true);
    let v = vec![headers, body];
    v
}
