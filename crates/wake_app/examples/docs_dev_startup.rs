//! Measure the public Docs service boundary, excluding component builds and metadata preparation.
//! Run with `cargo run --release -p wake_app --example docs_dev_startup -- <docs-project>`.
//! An optional second argument names an MDX page to temporarily edit and restore byte-for-byte.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use wake_app::{
    DevServer, DevServerEvent, DevServerOptions, ProjectOptions, start_docs_dev_server,
};

fn await_build(
    server: &DevServer,
    started: Instant,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    while started.elapsed() < Duration::from_secs(120) {
        for event in server.drain_events() {
            match event {
                DevServerEvent::Rebuilt { .. } => {
                    return Ok(serde_json::json!({
                        "feedbackMs": started.elapsed().as_secs_f64() * 1000.0,
                        "event": event,
                    }));
                }
                DevServerEvent::Diagnostic { .. } => {
                    return Err(format!("edit diagnostic: {event:?}").into());
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err("timed out waiting for a rebuild".into())
}

fn measure_edit(
    server: &DevServer,
    root: &Path,
    page: &Path,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let root = root.canonicalize()?;
    let page = root.join(page).canonicalize()?;
    if !page.starts_with(&root)
        || page.extension().and_then(|extension| extension.to_str()) != Some("mdx")
    {
        return Err("edit target must be an MDX page inside the measured project".into());
    }
    let original = std::fs::read(&page)?;
    std::str::from_utf8(&original)?;
    let mut modified = original.clone();
    modified.extend_from_slice(b"\n\nWake incremental performance measurement.\n");
    let started = Instant::now();
    std::fs::write(&page, &modified)?;
    let measured = await_build(server, started);
    // Never overwrite an edit from another process. Even on a build failure, restore our own
    // temporary change before reporting the failure to the caller.
    if std::fs::read(&page)? != modified {
        return Err("page changed concurrently; refusing to overwrite the new contents".into());
    }
    let restored_at = Instant::now();
    std::fs::write(&page, &original)?;
    let restored = await_build(server, restored_at);
    let measured = measured?;
    restored?;
    Ok(measured)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("expected docs project path")?;
    let page = std::env::args_os().nth(2).map(PathBuf::from);
    let reservation = TcpListener::bind(("127.0.0.1", 0))?;
    let port = reservation.local_addr()?.port();
    drop(reservation);
    let started = Instant::now();
    let server = start_docs_dev_server(DevServerOptions {
        project: ProjectOptions {
            cwd: Some(root.clone()),
            config_path: None,
        },
        port: Some(port),
        open: Some(false),
        ..DevServerOptions::default()
    })?;
    let ready_ms = started.elapsed().as_secs_f64() * 1000.0;
    let events = server.drain_events();
    let mut requests = Vec::new();
    if let Ok(paths) = std::env::var("WAKE_DOCS_MEASURE_PATHS") {
        let address = server
            .url()
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap();
        for path in paths.split(';').filter(|path| !path.is_empty()) {
            let started = Instant::now();
            let mut stream = TcpStream::connect(address)?;
            stream.set_read_timeout(Some(Duration::from_secs(120)))?;
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
            )?;
            let mut response = Vec::new();
            stream.read_to_end(&mut response)?;
            let response = String::from_utf8_lossy(&response);
            requests.push(serde_json::json!({
                "path": path, "durationMs": started.elapsed().as_secs_f64() * 1000.0,
                "status": response.lines().next(), "bytes": response.len(),
                "events": server.drain_events(),
            }));
        }
    }
    if let Ok(seconds) = std::env::var("WAKE_DOCS_HOLD_SECONDS") {
        eprintln!("Docs browser verification: {}", server.url());
        std::thread::sleep(Duration::from_secs(seconds.parse()?));
    }
    let edit = page
        .as_deref()
        .map(|page| measure_edit(&server, &root, page))
        .transpose();
    server.close()?;
    let edit = edit?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "readyMs": ready_ms,
            "events": events,
            "edit": edit,
            "requests": requests,
        }))?
    );
    Ok(())
}
