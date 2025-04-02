use crate::stream::HysResponse;
use bytes::{Bytes, BytesMut};
use log::{error, info, trace};
use std::io;
use std::sync::mpsc::SendError;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::select;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;

const WRITE_BUFFER_SIZE: usize = 65535*100; //64kb*5
pub struct ProxyManager {
    //in case needs of reconnection
    url: String,
    connection: Option<Result<TcpStream, io::Error>>,
    sender: Sender<HysResponse>,
    receiver: Option<mpsc::Receiver<Bytes>>,
    stream: Option<TcpStream>,
    write_buffer: BytesMut,
}
impl ProxyManager {
    pub fn new(
        server_addr: String,
        sender: Sender<HysResponse>,
        receiver: mpsc::Receiver<Bytes>,
    ) -> Self {
        Self {
            url: server_addr,
            connection: None,
            sender,
            receiver: Some(receiver),
            stream: None,
            write_buffer: BytesMut::with_capacity(WRITE_BUFFER_SIZE),
        }
    }
    pub async fn start(&mut self) {
        let url = self.url.clone();
        let connect_attempt= TcpStream::connect(url).await;
        if connect_attempt.is_err() {
            error!("Failed to connect to the remote: {}", connect_attempt.err().unwrap());
            self.send_fin().await;
            return;
        }
        self.connection = Some(connect_attempt);
        let mut receiver = self.receiver.take().unwrap();
        if let Some(Ok(stream)) = self.connection.take() {
            self.stream = Some(stream);
         loop {
                select! {
                Some(bytes) = receiver.recv() => {
                    info!("Proxy unit received {} bytes payload from the client", bytes.len());
                    //Received data is no longer copied to the struct's buffer first
                    //but passed as an argument to the sending function.
                    if let Err(e) = self.send_to_server(bytes).await {
                        error!("Failed to forward to the remote: {}", e);
                            //TODO should be more robust
                        self.send_fin().await;
                        return;
                    }
                }
                _ = async {
                    self.stream.as_mut().unwrap().readable().await.unwrap();
                } => {
                    match self.recv().await {
                        Ok(bytes)=>{
                            self.send_to_channel(bytes).await.expect("Failed to send to the channel");
                        }
                        Err(e) => {
                            if e.kind() == io::ErrorKind::UnexpectedEof {
                                info!("Connection to the remote is request to be closed by the remote");
                                // Handle the connection closed case
                                if let Err(e) = self.stream.take().unwrap().shutdown().await {
                                    error!("Failed to shutdown the stream: {}", e);
                                 }
                                self.send_fin().await;
                                return;
                            } else if e.kind() == io::ErrorKind::WouldBlock
                            {
                                trace!("Connection to the remote is not ready for reading");
                                continue;
                            }else{
                                error!("Failed to receive from the remote: {}", e);
                            }
                        }
                    }
                }
            }
            }
        }
    }

    pub async fn recv(&mut self) -> Result<Bytes, io::Error> {
        if let Some(ref mut stream) = self.stream {
            match stream.try_read_buf(&mut self.write_buffer) {
                Ok(n) => {
                    if n > 0 {
                        info!("Received {} bytes from the remote", n);
                        assert!(WRITE_BUFFER_SIZE-self.write_buffer.len() >10);
                        let mut received = std::mem::replace(
                            &mut self.write_buffer,
                            BytesMut::with_capacity(WRITE_BUFFER_SIZE),
                        );
                        received.truncate(n);
                        Ok(received.freeze())
                    } else {
                         Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Connection closed",
                        ))
                    }
                }
                Err(e) => Err(e),
            }
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Stream not connected",
            ))
        }
    }

    pub async fn send_to_server(&mut self, bytes: Bytes) -> Result<(), io::Error> {
        match self.stream.as_mut().unwrap().write(&bytes).await {
            Ok(0) => {
                // Connection closed
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Connection closed",
                ))
            }
            Ok(n) => {
                info!("Sent {} bytes to the remote", n);
                Ok(())
            }
            Err(e) => return Err(e),
        }
    }
    //if the payload is empty, send fin channel to the channel
    pub async fn send_to_channel(&mut self, bytes: Bytes) -> Result<(), SendError<Bytes>> {
        let len = bytes.len();
        match self
            .sender
            .send(HysResponse {
                bytes,
                fin: len == 0,
            })
            .await
        {
            Ok(()) => {
                info!("Sent {} bytes to the channel", len);
                return Ok(());
            }
            Err(e) => {
                error!("Error sending to the channel: {}", e);
                e
            }
        };
        Ok(())
    }
    async fn send_fin(&mut self){
        if let Err(e) = self.send_to_channel(Bytes::new()).await {
            error!("Failed to send fin to the channel: {}", e);
        }
    }
}
