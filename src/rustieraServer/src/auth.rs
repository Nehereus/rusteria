use log::{error, info, warn};
use quiche::h3::NameValue;
use crate::structs::{Client, PartialResponse};
use libRustiera::proto::*;
use quiche::h3::Header;

//TODO: auth should always return a response regardless of the auth result;
// in case of success auth, it should return a success AuthResponse
// else return masq
pub fn auth(headers: &[Header])->PartialResponse{
	let ps = PartialResponse{
		headers: None,
		body: vec![],
		written: 0,
	};
	if let Ok(req)=AuthRequest::from_event_header(headers){
			if verify(req.auth_token){
				let auth_resp = AuthResponse{
					status: 233,
					udp_supported: false,
					server_rx: 0,
					rx_auto: true,
					padding: "8964"
				};
				PartialResponse {
				headers:	Some(auth_resp.to_headers()),
				body: vec![],
				written: 0,
			}
			}else{
				masquerade()
			}
	}else{
		masquerade()
	}
}
//TODO implement verification
fn verify(token: &str)->bool{
	return token.eq("8964");
}
//TODO implement masquerade
fn masquerade()->PartialResponse{
	let headers = vec![Header::new(b":status",200.to_string().as_bytes())];
	PartialResponse{
		headers: Some(headers),
		body: b"Hello World!".to_vec(),
		written: 0,
	}
}

