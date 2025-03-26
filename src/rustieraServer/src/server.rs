use env_logger;
use log::{debug, error, info, warn};
use mio::net::UdpSocket;
use quiche::h3::NameValue;
use quiche::{Config, Connection, ConnectionId};
use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::net;
use std::net::{SocketAddr, ToSocketAddrs};
use std::str::from_utf8;
use ring::rand::SystemRandom;
use crate::structs::{Client, PartialResponse};
use crate::auth::*;

const BINDING_ADDR_STR: &str = "127.0.0.1:8888";


type ClientMap = HashMap<quiche::ConnectionId<'static>, Client>;

fn new_config() -> Config {
	let mut config = Config::new(quiche::PROTOCOL_VERSION).expect("Error creating new config");
	config.set_application_protos(quiche::h3::APPLICATION_PROTOCOL).expect("Error setting application version");
	config.set_initial_max_streams_bidi(8);
	config.set_initial_max_streams_uni(8);
	config.set_initial_max_data(20 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_local(10 * 1024 * 1024);
	config.set_initial_max_stream_data_bidi_remote(10 * 1024 * 1024);
	config.set_initial_max_stream_data_uni(10 * 1024 * 1024);
	config.load_cert_chain_from_pem_file("/tmp/cert/cert.pem").unwrap();
	config.load_priv_key_from_pem_file("/tmp/cert/key.pem").unwrap();
	return config;
}

#[allow(clippy::too_many_lines)]
pub(crate) fn main() -> Result<(), Box<dyn std::error::Error>> {
	env_logger::init();
	let mut poll = mio::Poll::new().unwrap();
	let mut events = mio::Events::with_capacity(1024);

	let to: SocketAddr = BINDING_ADDR_STR.to_socket_addrs().unwrap().next().unwrap();
	let mut socket = UdpSocket::bind(to).expect("couldn't bind to address");
	poll.registry()
		.register(&mut socket, mio::Token(0), mio::Interest::READABLE)
		.unwrap();

	let mut buf = [0; 10 * 1024]; //10KB QUIC packet receiving may overflow
	let mut out = [0; 65535];

	let mut q_config = new_config();
	let h3_config = quiche::h3::Config::new().unwrap();
	let magic_number = SystemRandom::new();;
	let conn_id_seed =
		ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &magic_number).unwrap();
	let mut clients = ClientMap::new();

	loop {
		let timeout = clients.values().filter_map(|c| c.q_conn.timeout()).min();
		//block current thread
		poll.poll(&mut events, timeout).unwrap();

		'read: loop {
			if events.is_empty() {
				debug!("timed out");

				clients.values_mut().for_each(|c| c.q_conn.on_timeout());
				//end read loop
				break 'read;
			}
			let (len, from) = match socket.recv_from(&mut buf) {
				Ok(v) => v,

				Err(e) => {
					// There are no more UDP packets to read, so end the read
					// loop.
					if e.kind() == std::io::ErrorKind::WouldBlock {
						debug!("recv() would block");
						break 'read;
					}

					panic!("recv() failed: {:?}", e);
				}
			};
			debug!("socket receives {} bytes", len);

			let pkt_buf = &mut buf[..len];

			//parse QUIC header
			let hdr = match quiche::Header::from_slice(
				pkt_buf,
				quiche::MAX_CONN_ID_LEN,
			) {
				Ok(v) => v,

				Err(e) => {
					error!("Parsing packet header failed: {:?}", e);
					continue 'read;
				}
			};
			let conn_id = ring::hmac::sign(&conn_id_seed, &hdr.dcid);
			//truncate
			let conn_id = &conn_id.as_ref()[..quiche::MAX_CONN_ID_LEN];
			let conn_id = conn_id.to_vec().into();

			//if the packet is a resumption of an existing connection
			let client = if !clients.contains_key(&hdr.dcid) &&
				!clients.contains_key(&conn_id) {
				//not an existing connection and not initial packet
				if hdr.ty != quiche::Type::Initial {
					error!("Packet is not Initial");
					continue 'read;
				}


				//negotiate version
				if !quiche::version_is_supported(hdr.version) {
					warn!("Doing version negotiation");

					let len =
						quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut out)
							.unwrap();

					let out = &out[..len];

					if let Err(e) = socket.send_to(out, from) {
						if e.kind() == std::io::ErrorKind::WouldBlock {
							debug!("send() would block");
							break;
						}

						panic!("send() failed: {:?}", e);
					}
					continue 'read;
				}
				//outgoing scid
				let mut scid = [0; quiche::MAX_CONN_ID_LEN];
				scid.copy_from_slice(&conn_id);

				let scid = quiche::ConnectionId::from_ref(&scid);

				// For client's first attempt, retry token will not present
				let token = hdr.token.as_ref().unwrap();

				if token.is_empty() {
					warn!("Doing stateless retry");

					let new_token = mint_token(&hdr, &from);

					let len = quiche::retry(
						&hdr.scid,
						&hdr.dcid,
						&scid,
						&new_token,
						hdr.version,
						&mut out,
					)
						.unwrap();

					let out = &out[..len];

					//TODO: replace duplicated code
					if let Err(e) = socket.send_to(out, from) {
						if e.kind() == std::io::ErrorKind::WouldBlock {
							debug!("send() would block");
							break;
						}

						panic!("send() failed: {:?}", e);
					}
					continue 'read;
				}

				//after a turn of conversation, the client will resend a request with the token
				let odcid = validate_token(&from, token);

				if odcid.is_none() {
					error!("Invalid address validation token");
					continue 'read;
				}
				assert_eq!(scid.len(), hdr.dcid.len());

				// Reuse the source connection ID we sent in the Retry packet,
				// instead of changing it again.
				// at this point, the dcid is the scid we sent earlier in the one with token
				let scid = hdr.dcid.clone();

				debug!("New connection: dcid={:?} scid={:?}", hdr.dcid, scid);

				let mut q_conn = quiche::accept(
					&scid,
					odcid.as_ref(),
					to,
					from,
					&mut q_config,
				).unwrap();

				q_conn.set_keylog(Box::new(std::fs::File::create("/tmp/keylog").unwrap()));

				let client = Client {
					q_conn,
					http3_conn: None,
					partial_responses: HashMap::new(),
				};
				clients.insert(scid.clone(), client);

				clients.get_mut(&scid).unwrap()
			} else {
				//existing connection
				match clients.get_mut(&hdr.dcid) {
					Some(v) => v,

					None => clients.get_mut(&conn_id).unwrap(),
				}
			};

			let recv_info = quiche::RecvInfo { from, to };

			let read = match client.q_conn.recv(pkt_buf, recv_info) {
				Ok(v) => v,

				Err(e) => {
					error!("{} recv failed: {:?}", client.q_conn.trace_id(), e);
					continue 'read;
				},
			};

			debug!("{} processed {} bytes", client.q_conn.trace_id(), read);
			// Create a new HTTP/3 connection as soon as the QUIC connection
			// is established.
			if (client.q_conn.is_in_early_data() || client.q_conn.is_established()) &&
				client.http3_conn.is_none()
			{
				info!(
					"{} is in early data: {}; established: {}",
					client.q_conn.trace_id(), client.q_conn.is_in_early_data(),
					client.q_conn.is_established()
				);

				let h3_conn = match quiche::h3::Connection::with_transport(
					&mut client.q_conn,
					&h3_config,
				) {
					Ok(v) => {
						v
					},

					Err(e) => {
						error!("failed to create HTTP/3 connection: {}", e);
						continue 'read;
					},
				};

				// TODO: sanity check h3 connection before adding to map
				client.http3_conn = Some(h3_conn);
			}
			if client.http3_conn.is_some() {
				// send remaining packets
				// TODO rewrite it now takes O(n), where most of stream are not necessary
				for stream_id in client.q_conn.writable() {
					handle_writable(client, stream_id);
				}

				// Process HTTP/3 events.
				loop {
					let http3_conn = client.http3_conn.as_mut().unwrap();

					match http3_conn.poll(&mut client.q_conn) {
						//handle auth header
						Ok((
							   stream_id,
							   quiche::h3::Event::Headers { list, .. },
						   )) => {
							handle_request(
								client,
								stream_id,
								&list,
								"examples/root",
							);
						},
						//TODO: handle body
						Ok((stream_id, quiche::h3::Event::Data)) => {

							info!(
								"{} got data on stream id {}",
								client.q_conn.trace_id(),
								stream_id
							);
						},

						Ok((_stream_id, quiche::h3::Event::Finished)) => (),

						Ok((_stream_id, quiche::h3::Event::Reset { .. })) => (),

						Ok((
							   _prioritized_element_id,
							   quiche::h3::Event::PriorityUpdate,
						   )) => (),

						Ok((_goaway_id, quiche::h3::Event::GoAway)) => (),

						Err(quiche::h3::Error::Done) => {
							break;
						},

						Err(e) => {
							error!(
								"{} HTTP/3 error {:?}",
								client.q_conn.trace_id(),
								e
							);

							break;
						},
					}
				}
			}

			// Generate outgoing QUIC packets for all active connections and send
			// them on the UDP socket, until quiche reports that there are no more
			// packets to be sent.
			// create response, return if blocking, then resume sending at the beinging
			// of the loop
			for client in clients.values_mut() {
				loop {
					let (write, send_info) = match client.q_conn.send(&mut out) {
						Ok(v) => v,

						Err(quiche::Error::Done) => {
							debug!("{} done writing", client.q_conn.trace_id());
							break;
						},

						Err(e) => {
							error!("{} send failed: {:?}", client.q_conn.trace_id(), e);

							client.q_conn.close(false, 0x1, b"fail").ok();
							break;
						},
					};

					if let Err(e) = socket.send_to(&out[..write], send_info.to) {
						if e.kind() == std::io::ErrorKind::WouldBlock {
							debug!("send() would block");
							break;
						}

						panic!("send() failed: {:?}", e);
					}

					debug!("{} written {} bytes", client.q_conn.trace_id(), write);
				}
			}
		}
	}
}
		/// Generate a stateless retry token.
		///
		/// The token includes the static string `"quiche"` followed by the IP address
		/// of the client and by the original destination connection ID generated by the
		/// client.
		///
		/// Note that this function is only an example and doesn't do any cryptographic
		/// authenticate of the token. *It should not be used in production system*.
		fn mint_token(hdr: &quiche::Header, src: &net::SocketAddr) -> Vec<u8> {
			let mut token = Vec::new();

			token.extend_from_slice(b"8964");

			let addr = match src.ip() {
				std::net::IpAddr::V4(a) => a.octets().to_vec(),
				std::net::IpAddr::V6(a) => panic!("ipv6 not supported")
			};

			token.extend_from_slice(&addr);
			token.extend_from_slice(&hdr.dcid);

			token
		}

		fn validate_token<'a>(
			src: &net::SocketAddr, token: &'a [u8],
		) -> Option<quiche::ConnectionId<'a>> {
			if token.len() < 4 {
				return None;
			}
			if &token[..4] != b"8964" {
				return None;
			}

			let token = &token[4..];

			let addr = match src.ip() {
				std::net::IpAddr::V4(a) => a.octets().to_vec(),
				std::net::IpAddr::V6(a) => panic!("should not reach here")
			};

			if token.len() < addr.len() || &token[..addr.len()] != addr.as_slice() {
				return None;
			}

			Some(quiche::ConnectionId::from_ref(&token[addr.len()..]))
		}

		fn handle_request(
			client: &mut Client, stream_id: u64, headers: &[quiche::h3::Header],
			root: &str,
		) {
			let q_conn = &mut client.q_conn;
			let http3_conn = &mut client.http3_conn.as_mut().unwrap();

			info!(
			"{} got request {:?} on stream id {}",
			q_conn.trace_id(),
			hdrs_to_strings(headers),
			stream_id
		);

			// We decide the response based on headers alone, so stop reading the
			// request stream so that any body is ignored and pointless Data events
			// are not generated.
			// q_conn.stream_shutdown(stream_id, quiche::Shutdown::Read, 0)
			// 	.unwrap();

			let (headers, body) = build_response(root, headers);

			match http3_conn.send_response(q_conn, stream_id, &headers, false) {
				Ok(v) => v,

				Err(quiche::h3::Error::StreamBlocked) => {
					let response = PartialResponse {
						headers: Some(headers),
						body,
						written: 0,
					};

					client.partial_responses.insert(stream_id, response);
					return;
				},

				Err(e) => {
					error!("{} stream send failed {:?}", q_conn.trace_id(), e);
					return;
				},
			}

			let written = match http3_conn.send_body(q_conn, stream_id, &body, true) {
				Ok(v) => v,

				Err(quiche::h3::Error::Done) => 0,

				Err(e) => {
					error!("{} stream send failed {:?}", q_conn.trace_id(), e);
					return;
				},
			};

			if written < body.len() {
				let response = PartialResponse {
					headers: None,
					body,
					written,
				};

				client.partial_responses.insert(stream_id, response);
			}
		}

		/// Builds an HTTP/3 response given a request.
		fn build_response(
			root: &str, request_headers: &[quiche::h3::Header],
		) -> (Vec<quiche::h3::Header>, Vec<u8>) {
			let mut file_path = std::path::PathBuf::from(root);
			let mut path = std::path::Path::new("");
			let auth_resp = auth(&request_headers);

			(auth_resp.headers.unwrap(), auth_resp.body)
		}

		/// Handles newly writable streams.
		fn handle_writable(client: &mut Client, stream_id: u64) {
			let conn = &mut client.q_conn;
			let http3_conn = &mut client.http3_conn.as_mut().unwrap();

			debug!("{} stream {} is writable", conn.trace_id(), stream_id);

			if !client.partial_responses.contains_key(&stream_id) {
				return;
			}

			let resp = client.partial_responses.get_mut(&stream_id).unwrap();

			if let Some(ref headers) = resp.headers {
				match http3_conn.send_response(conn, stream_id, headers, false) {
					Ok(_) => (),

					Err(quiche::h3::Error::StreamBlocked) => {
						return;
					},

					Err(e) => {
						error!("{} stream send failed {:?}", conn.trace_id(), e);
						return;
					},
				}
			}

			resp.headers = None;

			let body = &resp.body[resp.written..];

			let written = match http3_conn.send_body(conn, stream_id, body, true) {
				Ok(v) => v,

				Err(quiche::h3::Error::Done) => 0,

				Err(e) => {
					client.partial_responses.remove(&stream_id);

					error!("{} stream send failed {:?}", conn.trace_id(), e);
					return;
				},
			};

			resp.written += written;
			//partial send
			if resp.written == resp.body.len() {
				client.partial_responses.remove(&stream_id);
			}else{
				info!(
					"{} stream {} not finished, {} bytes left",
					conn.trace_id(),
					stream_id,
					resp.body.len() - resp.written
				);
			}
		}

		pub fn hdrs_to_strings(hdrs: &[quiche::h3::Header]) -> Vec<(String, String)> {
			hdrs.iter()
				.map(|h| {
					let name = String::from_utf8_lossy(h.name()).to_string();
					let value = String::from_utf8_lossy(h.value()).to_string();

					(name, value)
				})
				.collect()
		}

