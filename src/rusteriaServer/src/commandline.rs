// src/rusteriaServer/src/commandline.rs
use clap::{Arg, Command};
use log::LevelFilter;
use std::fs::File;

pub const DEFAULT_HOSTNAME: &str = "0.0.0.0";
pub const DEFAULT_LISTEN_PORT: &str = "8888";
pub const DEFAULT_AUTH_TOKEN: &str = "password";
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_CERT_PATH: &str = "/tmp/cert/cert.pem";
pub const DEFAULT_KEY_PATH: &str = "/tmp/cert/key.pem";

pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub auth_token: String,
    pub addr: String,
    pub cert_path: String,
    pub key_path: String,
}

pub fn parse_args() -> ServerConfig {
    let matches = Command::new("rusteriaServer")
        .arg(Arg::new("host")
            .long("host")
            .value_parser(clap::value_parser!(String))
            .default_value(DEFAULT_HOSTNAME))
        .arg(Arg::new("port")
            .long("port")
            .value_parser(clap::value_parser!(String))
            .default_value(DEFAULT_LISTEN_PORT))
        .arg(Arg::new("auth-token")
            .long("auth-token")
            .value_parser(clap::value_parser!(String))
            .default_value(DEFAULT_AUTH_TOKEN)
            .help("Authentication token for client connections"))
        .arg(Arg::new("log-file")
            .long("log-file")
            .value_parser(clap::value_parser!(String))
            .help("File to write logs to (empty for stderr)"))
        .arg(Arg::new("log-level")
            .long("log-level")
            .value_parser(clap::value_parser!(String))
            .default_value(DEFAULT_LOG_LEVEL)
            .help("Log level: trace, debug, info, warn, error"))
        .arg(Arg::new("cert-path")
            .long("cert-path")
            .value_parser(clap::value_parser!(String))
            .default_value(DEFAULT_CERT_PATH)
            .help("Path to TLS certificate file"))
        .arg(Arg::new("key-path")
            .long("key-path")
            .value_parser(clap::value_parser!(String))
            .default_value(DEFAULT_KEY_PATH)
            .help("Path to TLS private key file"))
        .get_matches();

    // Configure logging
    setup_logging(&matches);

    // Parse server configuration
    let hostname = matches.get_one::<String>("host").unwrap().clone();
    let port_str = matches.get_one::<String>("port").unwrap();
    let port = port_str.parse::<u16>().expect("Invalid port number");
    let auth_token = matches.get_one::<String>("auth-token").unwrap().clone();
    let cert_path = matches.get_one::<String>("cert-path").unwrap().clone();
    let key_path = matches.get_one::<String>("key-path").unwrap().clone();
    let addr = format!("{}:{}", hostname, port);

    ServerConfig {
        host: hostname,
        port,
        auth_token,
        addr,
        cert_path,
        key_path,
    }
}

fn setup_logging(matches: &clap::ArgMatches) {
    let log_level = matches.get_one::<String>("log-level")
        .unwrap()
        .to_lowercase();

    let level = match log_level.as_str() {
        "trace" => LevelFilter::Trace,
        "debug" => LevelFilter::Debug,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => LevelFilter::Info,
    };

    let mut builder = env_logger::Builder::new();
    builder.filter_level(level);

    // If log file is specified, write to file instead of stderr
    if let Some(log_file) = matches.get_one::<String>("log-file") {
        match File::create(log_file) {
            Ok(file) => {
                builder.target(env_logger::Target::Pipe(Box::new(file)));
            },
            Err(e) => {
                eprintln!("Failed to create log file {}: {}", log_file, e);
                // Fall back to stderr
            }
        }
    }

    builder.init();
}
