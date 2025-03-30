use std::collections::BTreeMap;
use tokio::net::TcpStream;

pub struct stream_handler {
    verified: bool,
    server_stream: Option<TcpStream>,
}
impl stream_handler {
    pub fn new(url: String) {}
}
