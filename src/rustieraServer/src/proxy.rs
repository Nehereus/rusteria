use std::io;
use bytes::BytesMut;
use tokio::net::TcpStream;
use tokio::runtime;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

struct TCPProxyManager {
	//in case needs of reconnection
	connection_handle: Option<JoinHandle<Result<TcpStream, io::Error>>>,
	stream: Option<TcpStream>,
	client_outbound_buffer: BytesMut,
	server_outbound_buffer: BytesMut,
	client_outbound_handle: Option<tokio::task::JoinHandle<()>>,
	server_outbound_handle: Option<tokio::task::JoinHandle<()>>,
	runtime: Runtime,
}
impl TCPProxyManager {
	pub fn new(server_addr: String) -> Self {
		let connection_handle = tokio::spawn(TcpStream::connect(server_addr.clone()));
		Self {
			connection_handle: Some(connection_handle),
			stream: None,
			client_outbound_buffer: BytesMut::with_capacity(65535),
			server_outbound_buffer: BytesMut::with_capacity(65535),
			client_outbound_handle: None,
			server_outbound_handle: None,
			runtime: runtime::Builder::new_current_thread().build().unwrap(),
		}
	}
}
