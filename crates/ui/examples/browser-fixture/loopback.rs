//! Minimal loopback HTTP server for browser fixture scenarios.
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::Duration,
};

pub struct LoopbackSite {
    pub origin: String,
    pub port: u16,
}

pub fn persistence_port(root: &Path) -> Option<u16> {
    if let Ok(port) = std::env::var("ZERON_BROWSER_PERSISTENCE_PORT") {
        return port.parse().ok();
    }
    let marker = root.join("relaunch-marker.json");
    if !marker.is_file() {
        return None;
    }
    let contents = std::fs::read_to_string(marker).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    value.get("port")?.as_u64().and_then(|p| u16::try_from(p).ok())
}

pub fn start(port: Option<u16>) -> anyhow::Result<LoopbackSite> {
    let listener = if let Some(port) = port {
        TcpListener::bind(format!("127.0.0.1:{port}"))
    } else {
        TcpListener::bind("127.0.0.1:0")
    }?;
    let addr = listener.local_addr()?;
    let origin = format!("http://{}", addr);
    thread::spawn(move || serve(listener));
    Ok(LoopbackSite {
        origin,
        port: addr.port(),
    })
}

fn serve(listener: TcpListener) {
    for mut stream in listener.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut request = [0; 4096];
        let n = stream.read(&mut request).unwrap_or(0);
        let request = String::from_utf8_lossy(&request[..n]);
        let (title, html) = if request.starts_with("GET /two ") {
            (
                "Details",
                "<a href='/'>Back to overview</a><h1>A closer look.</h1><p>Independent navigation, right beside your work.</p>",
            )
        } else {
            (
                "Fieldnotes",
                "<div class='eyebrow'>FIELDNOTES / WORKSPACE</div><h1>Make room<br>for good work.</h1><p>A quieter place to collect ideas, follow your progress, and build something that matters.</p><a class='button' id='details' href='/two'>Explore the workspace →</a><div class='cards'><article><small>01 / COLLECT</small><h2>Keep the good ideas.</h2><p>One place for the things you want to come back to.</p></article><article><small>02 / CREATE</small><h2>Find your next step.</h2><p>Small, thoughtful progress. Every single day.</p></article></div>",
            )
        };
        let body = format!(
            "<!doctype html><meta charset=utf-8><meta name='viewport' content='width=device-width'><title>{title}</title><style>body{{margin:0;padding:42px 32px;background:#f5f2eb;color:#263d35;font:15px/1.6 system-ui}}.eyebrow,small{{font-size:10px;letter-spacing:2px;color:#6d7c70}}h1{{font:500 45px/1.1 Georgia;margin:30px 0 20px}}p{{color:#6d776f;max-width:350px}}a{{color:inherit}}.button{{display:inline-block;margin:14px 0 30px;padding:10px 17px;background:#29483b;color:#fff;border-radius:7px;text-decoration:none;font-size:12px}}.cards{{display:grid;gap:14px}}article{{border:1px solid #d9ddd0;padding:20px;border-radius:10px}}h2{{font:500 21px Georgia;margin:12px 0}}article p{{font-size:12px;margin-bottom:0}}</style>{html}"
        );
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
    }
}
