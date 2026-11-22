//! Embedding Kensaku in a Rust application.
//!
//!   cargo run -p kensaku-core --example embed -- <folder> <query>
//!
//! Uses a throwaway data directory under the system temp dir, owns the index
//! in-process with `Engine`, indexes `<folder>`, waits for the first scan and
//! prints the top hits. Swap `Engine::open()` for `Client::connect()` to talk
//! to a `kensaku` daemon instead.

use kensaku_core::api::{Engine, Kensaku};
use kensaku_core::ops::setup::SetupRequest;
use std::time::{Duration, Instant};

const SCAN_TIMEOUT: Duration = Duration::from_secs(300);
/// A tiny folder can finish before the first poll; after this long, idle
/// counts as done even if no running state was observed.
const START_GRACE: Duration = Duration::from_secs(5);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(folder), Some(query)) = (args.next(), args.next()) else {
        eprintln!("usage: embed <folder> <query>");
        std::process::exit(2);
    };

    let data_dir = std::env::temp_dir().join("kensaku-embed-example");
    // SAFETY: single-threaded at this point; the engine reads it on open.
    unsafe { std::env::set_var("KENSAKU_DATA_DIR", &data_dir) };

    let engine = Engine::open()?;
    engine.set_status_listener(std::sync::Arc::new(|status| {
        eprintln!("[{}] {} files", status.phase, status.files_indexed);
    }));
    engine.setup(&SetupRequest {
        watched_dirs: vec![folder],
        ..Default::default()
    })?;

    // Wait for the first crawl to begin and finish.
    let started = Instant::now();
    let mut seen_running = false;
    while started.elapsed() < SCAN_TIMEOUT {
        let status = engine.status()?;
        seen_running |= status.is_indexing;
        if !status.is_indexing && (seen_running || started.elapsed() > START_GRACE) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let page = engine.search(&query, 10, 0)?;
    for hit in page.results {
        println!("{:.3}\t{}", hit.score, hit.path);
    }
    println!("data dir: {}", data_dir.display());
    Ok(())
}
