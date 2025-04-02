mod server;
mod auth;
mod hysteria;
mod proxy;
mod stream;
mod commandline;

use crate::server::server;
fn main() {
    server();
}