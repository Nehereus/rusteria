use std::collections::HashMap;

pub struct PartialResponse {
    pub headers: Option<Vec<quiche::h3::Header>>,

    pub body: Vec<u8>,

    pub written: usize,
}

pub struct Client {
    pub q_conn: quiche::Connection,

    pub http3_conn: Option<quiche::h3::Connection>,

    pub partial_responses: HashMap<u64, PartialResponse>,
}
