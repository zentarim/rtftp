mod cursor;
mod fs;
mod fs_watch;
mod guestfs;
pub mod local_fs;
mod messages;
mod nbd_disk;
mod options;
mod peer_handler;
mod remote_fs;
mod server;

use crate::fs_watch::Watch;
use clap::Parser;
use server::TFTPServer;
use std::path::PathBuf;
use std::process::ExitCode;
use std::string::String;
use std::time::Duration;
use tokio::runtime::Builder;
use tokio::task::LocalSet;

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
        short = 'm',
        long,
        help = "Monitor configs directory",
        default_value_t = true,
        long_help = "Monitor the TFTP root directory in real time and immediately open a remote FS at configs appearance."
    )]
    monitor_configs: bool,

    #[arg(
        short = 't',
        long,
        help = "Peer handler inactivity timeout",
        long_help = "After reaching this timeout of inactivity, a connected remote disk is closed."
    )]
    idle_timeout: u64,
}

fn main() -> ExitCode {
    LocalSet::new().block_on(
        &Builder::new_current_thread().enable_all().build().unwrap(),
        async_main(),
    )
}

async fn async_main() -> ExitCode {
    let args = Args::parse();
    let socket = match tokio::net::UdpSocket::bind((args.listen_ip, args.listen_port)).await {
        Ok(udp_socket) => udp_socket,
        Err(error) => {
            eprintln!("Socket bind error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let turn_duration = Duration::from_secs(1);
    let mut server = TFTPServer::new(socket, args.root_dir.clone(), args.idle_timeout);
    if args.monitor_configs {
        let monitor_directory = args.root_dir.to_string_lossy();
        let watch = match Watch::new().change().observe(&monitor_directory) {
            Ok(watch) => watch,
            Err(error) => {
                eprintln!("Failed to start watching directory {monitor_directory}: {error}");
                return ExitCode::FAILURE;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => eprintln!("Received SIGINT, shutting down"),
            _ = server.serve_augmented(turn_duration, &watch) => {}
        }
    } else {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => eprintln!("Received SIGINT, shutting down"),
            _ = server.serve(turn_duration) => {}
        }
    }
    eprintln!("Server is shut down");
    ExitCode::SUCCESS
}
