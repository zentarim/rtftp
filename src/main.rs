#[cfg(test)]
mod _tests;
mod cursor;
mod fs;
mod guestfs;
pub mod local_fs;
mod messages;
mod nbd_disk;
mod options;
mod peer_handler;
mod remote_fs;
mod server;
mod shutdown_handler;

use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::ExitCode;
use std::string::String;

use clap::Parser;
use server::TFTPServer;
use shutdown_handler::register_shutdown_flag;

#[derive(Parser, Debug)]
#[command(color = clap::ColorChoice::Never)]
struct Args {
    #[arg(short = 'l', long, help = "Listen IP")]
    listen_ip: String,

    #[arg(short = 'p', long, default_value_t = 69, help = "Listen port")]
    listen_port: u16,

    #[arg(
        short = 'r',
        long,
        help = "TFTP root directory",
        long_help = "A directory to serve files from"
    )]
    root_dir: PathBuf,

    #[arg(
        short = 't',
        long,
        help = "Peer handler inactivity timeout",
        long_help = "After reaching this timeout of inactivity, a connected remote disk is closed."
    )]
    idle_timeout: u64,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let shutdown_requested_flag = register_shutdown_flag()
        .unwrap_or_else(|error| panic!("Shutdown flag register error: {error}"));
    let socket = match UdpSocket::bind((args.listen_ip, args.listen_port)) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("Socket bind error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut server = TFTPServer::new(socket, args.root_dir, args.idle_timeout);
    if let Err(error) = server.serve_until_shutdown(shutdown_requested_flag) {
        eprintln!("Unknown error occurred: {error}");
        ExitCode::FAILURE
    } else {
        eprintln!("Exited");
        ExitCode::SUCCESS
    }
}
