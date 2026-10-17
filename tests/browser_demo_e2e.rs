//! The demo serves only synthetic static assets on loopback; no browser is opened.
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};
#[tokio::test]
async fn browser_demo_starts_on_loopback_and_cannot_serve_repository_files() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/browser-demo/serve.py");
    let invalid = tokio::process::Command::new("python3")
        .arg(&script)
        .args(["--port", "0"])
        .output()
        .await
        .unwrap();
    assert!(!invalid.status.success());
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut server = tokio::process::Command::new("python3")
        .arg(script)
        .args(["--port", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(server.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(Duration::from_secs(3), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(ready.contains(&port.to_string()));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let page = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(page.headers()["cache-control"], "no-store");
    let page = page.text().await.unwrap();
    assert!(page.contains("synthetic project information"));
    assert!(page.contains("data-jarvis-private"));
    for path in ["/app.js", "/style.css"] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    for path in [
        "/.env",
        "/Cargo.toml",
        "/../Cargo.toml",
        "/%2e%2e/.env",
        "/serve.py",
    ] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "served {path}"
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/"))
            .body("must not write")
            .send()
            .await
            .unwrap()
            .status(),
        501
    );
    server.kill().await.unwrap();
    let output = server.wait_with_output().await.unwrap();
    assert!(
        output.stderr.is_empty(),
        "demo logged request text or errors"
    );
}
