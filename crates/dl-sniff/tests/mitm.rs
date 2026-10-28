//! MITM/透传集成测试：本地起 TLS 源站，host_map 把 finder.example 域指过去，
//! 客户端走代理 CONNECT。验证：解密白名单域（捕获媒体+配对 key）、非白名单域
//! 纯透传（不产生条目）、CA 持久化加载确定性。

use std::collections::HashMap;
use std::sync::Arc;

use rcgen::{CertificateParams, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

use dl_sniff::{start, Capture, SniffConfig};

/// 本地 TLS 源站：`/api/feed` 回带 decode_key+直链的 JSON；`/media/v.mp4` 回固定字节。
async fn spawn_origin(dns_name: &str) -> (std::net::SocketAddr, String) {
    let key = KeyPair::generate().unwrap();
    let params = CertificateParams::new(vec![dns_name.to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let cert_der = cert.der().to_vec();
    let pem = cert.pem();
    let cfg = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der)],
            PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        )
        .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let acceptor = TlsAcceptor::from(Arc::new(cfg.clone()));
            tokio::spawn(async move {
                let mut tls = match acceptor.accept(stream).await {
                    Ok(t) => t,
                    Err(_) => return,
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // 读到头 + CL 体
                loop {
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_string();
                        let cl = head
                            .to_ascii_lowercase()
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= p + 4 + cl {
                            let req_line = head.lines().next().unwrap_or("").to_string();
                            let path = req_line.split(' ').nth(1).unwrap_or("/");
                            let (ct, body): (&str, Vec<u8>) = if path.contains("api/feed") {
                                (
                                    "application/json",
                                    r#"{"data":{"object_desc":"测试视频","decode_key":"2136343393","media":{"url":"https://finder.video.qq.com/media/v.mp4?sig=1"}}}"#
                                        .as_bytes()
                                        .to_vec(),
                                )
                            } else {
                                ("video/mp4", b"FAKEMP4BODY-0123456789".to_vec())
                            };
                            let mut resp = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                ct,
                                body.len()
                            )
                            .into_bytes();
                            resp.extend_from_slice(&body);
                            let _ = tls.write_all(&resp).await;
                            let _ = tls.shutdown().await;
                            return;
                        }
                    }
                    match tls.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
            });
        }
    });
    (addr, pem)
}

fn client_with(proxy_port: u16, extra_roots: &[String]) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{proxy_port}")).unwrap());
    for pem in extra_roots {
        b = b.add_root_certificate(reqwest::tls::Certificate::from_pem(pem.as_bytes()).unwrap());
    }
    b.build().unwrap()
}

#[tokio::test]
async fn mitm_captures_media_and_pairs_key() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::DEBUG)
        .with_target(false)
        .try_init();
    let tmp = tempfile::tempdir().unwrap();
    let (origin, _origin_pem) = spawn_origin("finder.video.qq.com").await;

    let mut cfg = SniffConfig::new(tmp.path().to_path_buf());
    cfg.port = 0; // 随机端口
    cfg.insecure_upstream = true; // 源站自签
    cfg.host_map = HashMap::from([("finder.video.qq.com".to_string(), origin)]);
    let capture = Arc::new(Capture::new(10));
    let svc = start(cfg, capture.clone()).await.unwrap();
    let ca_pem = std::fs::read_to_string(tmp.path().join("sniff/ca-cert.pem")).unwrap();

    let client = client_with(svc.port, &[ca_pem]);

    // 1. API（decode_key + 直链同响应 → 直接成条配对）
    let api = client
        .post("https://finder.video.qq.com/api/feed")
        .header("content-type", "application/json")
        .body(r#"{"cursor":"0"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(api.status(), 200);
    assert!(api.text().await.unwrap().contains("decode_key"));

    // 2. 媒体直链（Range 场景外的整段 GET）
    let media = client
        .get("https://finder.video.qq.com/media/v.mp4?sig=1")
        .header("referer", "https://finder.video.qq.com/")
        .send()
        .await
        .unwrap();
    assert_eq!(media.status(), 200);
    let bytes = media.bytes().await.unwrap();
    assert_eq!(&bytes[..11], b"FAKEMP4BODY");

    let items = capture.items();
    assert_eq!(
        items.len(),
        1,
        "同 URL 去重后 1 条: {:?}",
        items.iter().map(|i| &i.url).collect::<Vec<_>>()
    );
    let it = &items[0];
    assert_eq!(it.url, "https://finder.video.qq.com/media/v.mp4?sig=1");
    assert_eq!(it.seed.as_deref(), Some("2136343393"), "decode_key 配对");
    assert_eq!(it.bytes, Some(22));
    assert_eq!(
        it.headers.get("referer").map(|s| s.as_str()),
        Some("https://finder.video.qq.com/")
    );
    assert_eq!(it.app, "wechat");
    svc.stop();
}

#[tokio::test]
async fn passthrough_does_not_capture() {
    let tmp = tempfile::tempdir().unwrap();
    let (origin, origin_pem) = spawn_origin("example.org").await;

    let mut cfg = SniffConfig::new(tmp.path().to_path_buf());
    cfg.port = 0;
    cfg.insecure_upstream = true;
    cfg.host_map = HashMap::from([("example.org".to_string(), origin)]);
    let capture = Arc::new(Capture::new(10));
    let ca_pem = std::fs::read_to_string(tmp.path().join("sniff/ca-cert.pem")).unwrap_or_default();
    let svc = start(cfg, capture.clone()).await.unwrap();

    // 透传：客户端直信源站证书（若被解密会因不信任 MITM CA 而失败）
    let client = client_with(svc.port, &[origin_pem, ca_pem]);
    let resp = client
        .get("https://example.org/media/v.mp4")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().len(), 22);
    assert!(capture.items().is_empty(), "非白名单域不产生条目");
    svc.stop();
}

#[tokio::test]
async fn ca_persists_across_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let a = dl_sniff::ca::SniffCa::load_or_create(&tmp.path().join("sniff")).unwrap();
    // ECDSA 自签随机化 → PEM 字节可不同；关键保证是「重启后签的叶子仍能被
    // 已安装的原 CA 证书校验」（Subject/公钥/SKI 一致）。以 rustls 握手验证。
    let b = dl_sniff::ca::SniffCa::load_or_create(&tmp.path().join("sniff")).unwrap();
    assert!(
        handshake_ok(&a, &b).await,
        "A 签的叶子须通过 B(=已装信任) 的校验"
    );
    assert!(handshake_ok(&b, &a).await, "对称方向同样成立");
}

/// 用 server=sniff 的叶子配置起 TLS 监听，client 信任 root_of 的 CA 证书握手。
async fn handshake_ok(server: &dl_sniff::ca::SniffCa, root_of: &dl_sniff::ca::SniffCa) -> bool {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_cfg = server.leaf_config("probe.example").unwrap();
    let server_task = tokio::spawn(async move {
        if let Ok((s, _)) = listener.accept().await {
            let _ = tokio_rustls::TlsAcceptor::from(server_cfg).accept(s).await;
        }
    });
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    let pem = root_of.cert_pem();
    for c in rustls_pemfile::certs(&mut pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let client_cfg = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let conn = tokio::net::TcpStream::connect(addr).await.unwrap();
    let dns =
        tokio_rustls::rustls::pki_types::ServerName::try_from("probe.example".to_string()).unwrap();
    let ok = tokio_rustls::TlsConnector::from(Arc::new(client_cfg))
        .connect(dns, conn)
        .await
        .is_ok();
    let _ = server_task.await;
    ok
}
