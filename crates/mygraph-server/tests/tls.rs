//! HTTPS end to end: the server with a certificate from `[tls]`, the clients of this binary
//! (the stdio bridge's and `mygraph health`'s), and reloading a renewed certificate.

use mygraph_server::config::{Config, TlsConfig};
use mygraph_server::{App, tls};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A self-signed certificate for `names`, written as `<stem>.pem` and `<stem>.key` in `dir`.
fn certificate(dir: &Path, stem: &str, names: &[&str]) -> (PathBuf, PathBuf) {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let made = rcgen::generate_simple_self_signed(names).unwrap();
    let cert = dir.join(format!("{stem}.pem"));
    let key = dir.join(format!("{stem}.key"));
    std::fs::write(&cert, made.cert.pem()).unwrap();
    std::fs::write(&key, made.signing_key.serialize_pem()).unwrap();
    (cert, key)
}

fn health(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    agent
        .get(&format!("{url}/health"))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

#[test]
fn serves_https_with_the_configured_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = certificate(dir.path(), "server", &["localhost"]);
    let (other, _) = certificate(dir.path(), "other", &["localhost"]);
    let mut config = Config {
        data_dir: dir.path().join("data"),
        tls: TlsConfig {
            cert: Some(cert.clone()),
            key: Some(key),
        },
        ..Config::default()
    };
    config.search.semantic = false;
    config.validate().unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = {
        let _guard = rt.enter();
        App::new(config)
    };
    rt.spawn(mygraph_server::serve_listener(app, listener));

    // The certificate is trusted through the CA file, as with `--ca-cert`.
    let trusting = tls::client(Some(&cert)).unwrap();
    let url = format!("https://localhost:{port}");
    let started = Instant::now();
    let body = loop {
        match health(&trusting, &url) {
            Ok(body) => break body,
            Err(e) if started.elapsed() > Duration::from_secs(20) => panic!("{e}"),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    assert!(body.contains(r#""status":"ok""#), "{body}");

    // Not trusted with the public roots only, nor with another certificate.
    let err = health(&tls::client(None).unwrap(), &url).unwrap_err();
    assert!(err.to_lowercase().contains("certificate"), "{err}");
    assert!(health(&tls::client(Some(&other)).unwrap(), &url).is_err());

    // Plain HTTP on the HTTPS port is told to use HTTPS ([API-HTTP-4]).
    match ureq::agent()
        .post(&format!("http://127.0.0.1:{port}/v1/graphs/x/search"))
        .send_string(&"x".repeat(100_000))
    {
        Err(ureq::Error::Status(400, r)) => {
            let body = r.into_string().unwrap();
            assert!(body.contains(r#""code":"api.https_required""#), "{body}");
            assert!(body.contains("use https://"), "{body}");
        }
        other => panic!("{other:?}"),
    }
    assert!(health(&ureq::agent(), &format!("http://127.0.0.1:{port}")).is_err());

    // `mygraph health` asks the loopback address, which the certificate does not name.
    assert!(health(&trusting, &format!("https://127.0.0.1:{port}")).is_err());
    let body = health(
        &tls::loopback_client(),
        &format!("https://127.0.0.1:{port}"),
    )
    .unwrap();
    assert!(body.contains(r#""status":"ok""#), "{body}");

    assert!(tls::explain("invalid peer certificate: CaUsedAsEndEntity").contains("CA:FALSE"));

    // A CA file without certificates is refused.
    let empty = dir.path().join("empty.pem");
    std::fs::write(&empty, "").unwrap();
    assert!(
        tls::client(Some(&empty))
            .unwrap_err()
            .contains("no certificate")
    );
}

#[test]
fn a_renewed_certificate_is_served_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = certificate(dir.path(), "live", &["localhost"]);
    let old = dir.path().join("old.pem");
    std::fs::copy(&cert, &old).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // As serve_listener does: tokio needs a non-blocking socket.
    listener.set_nonblocking(true).unwrap();
    let (cert2, key2) = (cert.clone(), key.clone());
    rt.spawn(async move {
        let config = tls::server_config(&cert2, &key2).await.unwrap();
        tls::spawn_reload(config.clone(), cert2, key2, Duration::from_millis(50));
        let app = axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }));
        axum_server::from_tcp_rustls(listener, config)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });
    let url = format!("https://localhost:{port}");
    let wait_for = |trust: &Path| {
        let agent = tls::client(Some(trust)).unwrap();
        let started = Instant::now();
        loop {
            match health(&agent, &url) {
                Ok(_) => return,
                Err(e) if started.elapsed() > Duration::from_secs(20) => panic!("{e}"),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    };
    wait_for(&old);

    // Renewal replaces both files; a key that does not load yet keeps the old pair.
    std::fs::write(&key, "not a key").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    wait_for(&old);
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    std::fs::write(&cert, made.cert.pem()).unwrap();
    std::fs::write(&key, made.signing_key.serialize_pem()).unwrap();
    wait_for(&cert);
    assert!(health(&tls::client(Some(&old)).unwrap(), &url).is_err());
}
