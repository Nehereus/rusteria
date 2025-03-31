use bytes::buf::Reader;
use bytes::{Buf, Bytes, BytesMut};
use std::io;
use std::io::{Error, Read};
use std::sync::mpsc::SendError;
use log::{error, info, warn};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::{runtime, select};

pub struct ProxyManager {
    //in case needs of reconnection
    url: String,
    connection: Option<Result<TcpStream, io::Error>>,
    sender: mpsc::Sender<Bytes>,
    receiver: Option<mpsc::Receiver<Bytes>>,
    stream: Option<TcpStream>,
    read_buffer: BytesMut,
    write_buffer: BytesMut,
}
impl ProxyManager {
    pub fn new(
        server_addr: String,
        sender: mpsc::Sender<Bytes>,
        receiver: mpsc::Receiver<Bytes>,
    ) -> Self {
        Self {
            url: server_addr,
            connection: None,
            sender,
            receiver:Some(receiver),
            stream: None,
            read_buffer: BytesMut::with_capacity(65535),
            write_buffer: BytesMut::with_capacity(65535),
        }
    }
    pub async fn start(&mut self) {
        let url = self.url.clone();
        self.connection = Some(TcpStream::connect(url).await);
        let mut receiver = self.receiver.take().unwrap();
        if let Some(Ok(stream)) = self.connection.take() {
            self.stream = Some(stream);
            loop {
                select! {
                    Some(bytes) = receiver.recv() => {
                        info!("Received bytes from channel: {:?}", bytes);
                        self.read_buffer.extend_from_slice(&bytes);
                        if let Err(e) = self.send_to_target().await {
                            error!("Send error: {}", e);
                            break;
                        }
                    }
                    _ = async {
                        self.stream.as_mut().unwrap().readable().await.unwrap();
                    } => {
                        match self.recv().await {
                            Ok(())=>{
                                self.send_to_channel().await.expect("TODO: panic message");
                            }
                            Err(e) => {
                                if e.kind() == io::ErrorKind::UnexpectedEof {
                                    info!("Connection closed");
                                    // Handle the connection closed case
                                    self.stream.take().unwrap().shutdown().await;
                                    break;
                                } else {
                                    error!("Receive error: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }
    }


    pub async fn recv(&mut self) -> Result<(), io::Error> {
        if let Some(ref mut stream) = self.stream {
            let mut buf = vec![0u8; 8192];
            match stream.read(&mut buf).await {
                Ok(n)  => {
                    if n>0 {
                        info!("Received {} bytes from remote", n);
                        self.write_buffer.extend_from_slice(&buf[..n]);
                        error!("New buffer length: {}", self.write_buffer.len());
                    }else{
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Connection closed"));
                    }
                }
                Err(e) => {
                    // Handle the error
                    error!("Error reading from stream: {}", e);
                    return Err(e);
                }

            }
        } else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "Stream not connected"));
        }
        Ok(())
    }

    pub async fn send_to_target(&mut self) -> Result<(), io::Error> {
        if let Some(ref mut stream) = self.stream {
            if !self.read_buffer.is_empty() {
                let bytes_to_send = self.read_buffer.len();

                match stream.write(&self.read_buffer[..bytes_to_send]).await {
                    Ok(0) => {
                        // Connection closed
                        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Connection closed"));
                    }
                    Ok(n) => {
                        // Remove the sent bytes from the buffer
                        info!("Sent {} bytes to target", n);
                        self.read_buffer.advance(n);
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
    pub async fn send_to_channel(&mut self) -> Result<(), SendError<Bytes>> {
        if let Some(ref mut stream) = self.stream {
            if !self.write_buffer.is_empty() {
                let bytes_to_send = self.write_buffer.len();
                match self.sender.send(Bytes::copy_from_slice(&self.write_buffer[..bytes_to_send])).await {
                    Ok(()) => {
                        info!("Sent {} bytes to channel", bytes_to_send);
                        return Ok(());
                    }
                    Err(e) =>{
                        error!("Error sending to channel: {}", e);
                        e
                    } 
                };
            }
        }
        Ok(())
    }
}
