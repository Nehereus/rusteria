# Rusteria
## What is this
This is a spring break project (which took my entire 2025 spring break) aimed at gaining exposure to Rust and QUIC/HTTP3. The goal is to implement a [Hysteria](https://github.com/apernet/hysteria) proxy server in Rust. Hysteria is a proxy [protocol](https://v2.hysteria.network/docs/developers/Protocol/) built on QUIC, mmasquerading as an HTTP3 web server to evade active probing.
The server is implemented using [Tokio](https://tokio.rs) asynchronous runtime, with [quiche](https://github.com/cloudflare/quiche) as the QUIC API. [Bytes](https://docs.rs/bytes/latest/bytes/) package package is used for most network buffer operations.
The package has two components. ```libRusteria``` is the implementation of the Hysteria protocol. ```serverRusteria``` is the executable. 

## What is implemented/not implemented
- [x] HTTP3 masquerading server
- [x] Authentication
- [x] TCP proxy
- [] UDP proxy
- [] Congestion control (currently using tokio-quiche's default)

## How things go
The program is a (long) weekend hack, which means it's not meant to be a daily driver proxy server. However, thanks to Rust's memory efficiency, the memory footprint of the app should be as small as ~30MB. In terms of performance, it's frankly underoptimized. In LAN test experiments, the peak throughput is ~200MB/s.

## How to use
### prerequisite 
A pair of X509 cert and key should be placed at ```/tmp/cert/cert.pem``` and ```/tmp/cert/key.pem``` or wherever specified by ```--cert-path``` and ```--key-path```.       
If you want to try it out, you may build and run the project as instructed below:      
```bash
git clone https://github.com/Nehereus/rusteria
# the executable is at the rusteriaServer module
cd rusteria/src/rusteriaServer/
cargo build -r
# back to the project root
cd ../..
# run
target/release/rusteriaServer
# or use cargo run, assuming you are now at the project root
cargo run --package rusteriaServer --bin rusteriaServer --release -- <options>
```
### Command Line Options
The Rusteria server supports the following command line options:

```
--host <ADDRESS>       Hostname or IP address to bind to (default: 0.0.0.0)
--port <PORT>          Port number to listen on (default: 8888)
--auth-token <TOKEN>   Authentication token for client connections (default: password)
--log-file <FILE>      File to write logs to (empty for stderr)
--log-level <LEVEL>    Log level: trace, debug, info, warn, error (default: info)
--cert-path <PATH>     Path to TLS certificate file (default: /tmp/cert/cert.pem)
--key-path <PATH>      Path to TLS private key file (default: /tmp/cert/key.pem)
```

