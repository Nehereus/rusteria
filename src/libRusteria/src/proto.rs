use log::{debug, error, info, warn};
use octets::Octets;
use quiche::Error;
use quiche::h3::{Header, NameValue};

const MAX_ADDRESS_LENGTH: u64 = 253;
pub struct AuthRequest<'a> {
    //Authentication credentials.
    pub auth_token: &'a str,
    //Client's maximum receive rate in bytes per second. A value of 0 indicates unknown
    pub client_rx: u64,
    //A random padding string of variable length
    pub padding: &'a str,
}

pub struct AuthResponse<'a> {
    pub status: u8, //233 for ok
    //whether UDP replay is supported on this server
    pub udp_supported: bool,
    //Server's maximum receive rate in bytes per second. A value of 0 indicates unlimited;
    // "auto" indicates the server refuses to provide a value and ask the client to use
    // congestion control to determine the rate on its own.
    pub server_rx: u64,
    pub rx_auto: bool,
    pub padding: &'a str,
}
impl<'a> AuthResponse<'a> {
    pub fn to_headers(&self) -> Vec<Header> {
        let mut headers = Vec::new();
        headers.push(Header::new(b":status", b"233"));
        headers.push(Header::new(
            b"Hysteria-UDP",
            self.udp_supported.to_string().as_bytes(),
        ));

        if self.rx_auto {
            headers.push(Header::new(b"Hysteria-CC-RX", b"auto"));
        } else {
            let server_rx_str = self.server_rx.to_string();
            headers.push(Header::new(b"Hysteria-CC-RX", server_rx_str.as_bytes()));
        }

        headers.push(Header::new(b"Hysteria-Padding", self.padding.as_bytes()));
        headers
    }
}

// [varint] 0x401 (TCPRequest ID)
// [varint] Address length
// [bytes] Address string (host:port)
// [varint] Padding length
// [bytes] Random padding
pub struct HysteriaTcpRequest {
    pub request_id: u64,
    //maximum possible length is 253 thus only reasonable int type is u8
    //potentially none resolved url
    pub url: String,
    pub padding: String,
    //the pos of the last byte in the input slice
    pub offset: usize,
}

//TODO error handling when you have time
impl HysteriaTcpRequest {
    pub fn from_bytes(bytes: &[u8]) -> Option< Self> {
        let mut octet = Octets::with_slice(bytes);
        let request_id = octet.get_varint().ok()?;
        let addr_len = octet.get_varint().ok()?;
        if addr_len == 0 || addr_len > MAX_ADDRESS_LENGTH {
            error!(
                "Failed to parse: the request has a url length of {}, which is invalid",
                addr_len
            );
            return None;
        }
        let url_bytes = octet.get_bytes(addr_len as usize).ok()?;
        let url = std::str::from_utf8(url_bytes.buf()).ok()?.to_string();

        let padding_len = octet.get_varint().ok()?;
        let padding_bytes = octet.get_bytes(padding_len as usize).ok()?;
        let padding = std::str::from_utf8(padding_bytes.buf()).ok()?.to_string();
        let offset=octet.off();
        info!(
            "Parsed TCP request: request_id={}, addr_len={}, url={}, padding_len={}, remaining_bytes={}",
            request_id, addr_len, url, padding_len, octet.len()-octet.off()
        );

        Some(Self {
            request_id,
            url,
            padding,
            offset,
        })
    }
}
// [uint8] Status (0x00 = OK, 0x01 = Error)
// [varint] Message length
// [bytes] Message string
// [varint] Padding length
// [bytes] Random padding
pub struct HysteriaTCPResponse {
    pub status: HysteriaTCPResponseStatus,
    msg_len: u8,
    pub msg: Vec<u8>,
    padding_len: u8,
    pub padding: Vec<u8>,
}
pub enum HysteriaTCPResponseStatus {
    Ok = 0x00,
    Error = 0x01,
}
impl HysteriaTCPResponse {
    pub fn new(status: HysteriaTCPResponseStatus, msg: &str, padding: &str) -> Self {
        Self {
            status,
            msg_len: msg.len() as u8,
            msg: msg.as_bytes().to_vec(),
            padding_len: padding.len() as u8,
            padding: padding.as_bytes().to_vec(),
        }
    }
    //TODO: msg seems not working on the client end
    pub fn into_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.push(self.status as u8);
        bytes.extend_from_slice(&self.msg_len.to_be_bytes());
        bytes.extend_from_slice(&self.msg);
        bytes.extend_from_slice(&self.padding_len.to_be_bytes());
        bytes.extend_from_slice(&self.padding);
        bytes
    }
}

// [uint32] Session ID
// [uint16] Packet ID
// [uint8] Fragment ID
// [uint8] Fragment count
// [varint] Address length
// [bytes] Address string (host:port)
// [bytes] Payload
pub struct UDPPacket {
    session_id: u32,
    packet_id: u16,
    frag_id: u8,
    frag_count: u8,
    addr_len: u16,
    ipv4addr: Vec<u8>,
    payload: Vec<u8>,
}

impl<'a> AuthRequest<'a> {
    pub fn from_event_header(headers: &'a [quiche::h3::Header]) -> Result<Self, Error> {
        let mut auth_token: Option<&'a str> = None;
        let mut client_rx: Option<u64> = None;
        let mut padding: Option<&'a str> = None;

        for header in headers {
            match std::str::from_utf8(header.name()) {
                Ok("hysteria-auth") => match std::str::from_utf8(header.value()) {
                    Ok(s) => auth_token = Some(s),
                    Err(e) => warn!("Invalid auth header: {}", e),
                },
                Ok("hysteria-cc-rx") => {
                    let rx_str =
                        std::str::from_utf8(header.value()).expect("failed to read client rx");
                    if rx_str == "auto" {
                        client_rx = Some(0);
                    } else {
                        client_rx = Some(
                            rx_str
                                .parse::<u64>()
                                .expect("failed to parse client rx to u64"),
                        );
                    }
                }

                Ok("hysteria-padding") => match std::str::from_utf8(header.value()) {
                    Ok(s) => padding = Some(s),
                    Err(e) => warn!("Invalid padding string: {}", e),
                },
                Ok(s) => {
                    debug!("Received unknown header: {}", s);
                }
                Err(e) => warn!("Failed to parse the header: {}", e),
            }
        }

        if let (Some(auth_token), Some(client_rx), Some(padding)) = (auth_token, client_rx, padding)
        {
            Ok(AuthRequest {
                auth_token,
                client_rx,
                padding,
            })
        } else {
            warn!("Incomplete auth headers");
            Err(Error::InvalidState)
        }
    }
}
