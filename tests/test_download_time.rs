use crate::common::client::timed_download;
use crate::common::{
    ensure_guest_initramfs, make_payload, run_nbd_server, start_rtftp_with_idle_timeout,
    wait_until_appliances_gone,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
mod common;

const SIZES_MIB: [usize; 6] = [1, 2, 4, 8, 16, 32];
const BLOCK_SIZES: [Option<u16>; 2] = [None, Some(1468)];
const MEASURED_RUNS: usize = 3;
const SOURCE_IP: &str = "127.0.0.11";

struct Run {
    first_response: Duration,
    total: Duration,
    max_gap: Duration,
    max_gap_block: usize,
    retransmits: usize,
}

impl Run {
    /// MiB/s over the part of the transfer that follows the first response.
    fn rate(&self, size_mib: usize) -> f64 {
        size_mib as f64 / (self.total - self.first_response).as_secs_f64()
    }
}

fn mode_name(block_size: Option<u16>) -> String {
    match block_size {
        None => "plain".to_string(),
        Some(block_size) => format!("blksize={block_size}"),
    }
}

fn report_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bench")
}

/// Count the server's retransmissions: an ACK timeout or a duplicate ACK
/// both make it send the window again.
fn count_retransmits(server_log: &str) -> usize {
    server_log
        .lines()
        .filter(|line| line.contains("Timeout waiting for") || line.contains("Duplicate ACK"))
        .count()
}

fn run_dir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("rtftp_{pid}_download_time_{label}"));
    fs::create_dir(&dir).unwrap();
    dir
}

/// Download one file from a freshly started rtftp, so the guest boots cold.
async fn measure_once(nbd_url: &str, size_mib: usize, block_size: Option<u16>, label: &str) -> Run {
    let server_dir = run_dir(label);
    let config = json!({
        "url": nbd_url,
        "mounts": [
            {
                "partition": 2,
                "mountpoint": "/",
            },
            {
                "partition": 1,
                "mountpoint": "/boot",
            }
        ],
        "tftp_root": "/boot",
    });
    fs::write(
        server_dir.join(format!("{SOURCE_IP}.nbd")),
        config.to_string(),
    )
    .unwrap();
    // A one-second idle timeout lets the guest exit by itself after the
    // transfer. Dropping the server first would orphan the guest.
    let running_server = start_rtftp_with_idle_timeout(server_dir, 1).await;
    let client = running_server.open_paired_client(SOURCE_IP).await;
    let file = format!("size_{size_mib}M.file");
    let downloaded = timed_download(client, &file, block_size)
        .await
        .unwrap_or_else(|err| panic!("{label}: {err}\n{}", running_server.stderr_output()));
    assert!(
        downloaded.data == make_payload(size_mib * 1048576),
        "{label}: downloaded {} bytes do not match the payload",
        downloaded.data.len()
    );
    wait_until_appliances_gone(running_server.pid()).await;
    let server_log = running_server.stderr_output();
    fs::write(
        report_dir().join("logs").join(format!("{label}.log")),
        &server_log,
    )
    .unwrap();
    Run {
        first_response: downloaded.first_response,
        total: downloaded.total,
        max_gap: downloaded.max_gap,
        max_gap_block: downloaded.max_gap_block,
        retransmits: count_retransmits(&server_log),
    }
}

fn spread(values: &mut [f64]) -> (f64, f64, f64) {
    values.sort_by(f64::total_cmp);
    (
        values[values.len() / 2],
        values[0],
        values[values.len() - 1],
    )
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "measurement; run with `make bench-download`"]
async fn measure_download_time() {
    ensure_guest_initramfs();
    // Server logs of an earlier measurement would mix with this one.
    let logs_dir = report_dir().join("logs");
    let _ = fs::remove_dir_all(&logs_dir);
    fs::create_dir_all(&logs_dir).unwrap();
    let nbd_process = run_nbd_server("127.0.0.2");
    let nbd_url = nbd_process.get_url();
    let mut table = Vec::new();
    let mut cases = Vec::new();
    for block_size in BLOCK_SIZES {
        for size_mib in SIZES_MIB {
            let mode = mode_name(block_size);
            let case = format!("{size_mib}M_{}", mode.replace('=', ""));
            // The first run warms the host page cache and is not reported.
            measure_once(nbd_url, size_mib, block_size, &format!("{case}_warmup")).await;
            let mut runs = Vec::with_capacity(MEASURED_RUNS);
            for run in 0..MEASURED_RUNS {
                runs.push(
                    measure_once(nbd_url, size_mib, block_size, &format!("{case}_{run}")).await,
                );
            }
            let total = spread(
                &mut runs
                    .iter()
                    .map(|r| r.total.as_secs_f64())
                    .collect::<Vec<_>>(),
            );
            let first = spread(
                &mut runs
                    .iter()
                    .map(|r| r.first_response.as_secs_f64())
                    .collect::<Vec<_>>(),
            );
            let rate = spread(&mut runs.iter().map(|r| r.rate(size_mib)).collect::<Vec<_>>());
            table.push(format!(
                "{size_mib:>5} {mode:<13} {:>8.3} {:>8.3} {:>8.3} {:>11.3} {:>9.2}",
                total.0, total.1, total.2, first.0, rate.0
            ));
            cases.push(json!({
                "size_mib": size_mib,
                "mode": mode,
                "runs": runs.iter().map(|r| json!({
                    "total_s": r.total.as_secs_f64(),
                    "first_response_s": r.first_response.as_secs_f64(),
                    "rate_mib_s": r.rate(size_mib),
                    "max_gap_s": r.max_gap.as_secs_f64(),
                    "max_gap_block": r.max_gap_block,
                    "retransmits": r.retransmits,
                })).collect::<Vec<_>>(),
                "median_total_s": total.0,
                "min_total_s": total.1,
                "max_total_s": total.2,
                "median_first_response_s": first.0,
                "median_rate_mib_s": rate.0,
            }));
        }
    }
    eprintln!("\nDownload time with a cold guest, median of {MEASURED_RUNS} runs (seconds):");
    eprintln!(" MiB  mode            total      min      max  first-resp     MiB/s");
    for row in &table {
        eprintln!("{row}");
    }
    let report = json!({
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "measured_runs": MEASURED_RUNS,
        "cases": cases,
    });
    let report_dir = report_dir();
    fs::create_dir_all(&report_dir).unwrap();
    let report_path = report_dir.join("download_time.json");
    fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!("Report: {}", report_path.display());
}
