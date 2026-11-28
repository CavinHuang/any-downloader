//! 网络诊断（出口 IP 识别 + 多线程下载测速），严格遵循传入的代理策略。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_error::{DownloadError, ErrorKind, Res};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpInfo {
    pub ip: String,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub isp: Option<String>,
    pub asn: Option<String>,
    pub latency_ms: u64,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedProgress {
    pub phase: String,
    pub elapsed_ms: u64,
    pub bytes: u64,
    pub current_bps: u64,
    pub avg_bps: u64,
    pub peak_bps: u64,
    pub latency_ms: Option<u64>,
    pub jitter_ms: Option<u64>,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedTestResult {
    pub server: String,
    pub latency_ms: Option<u64>,
    pub jitter_ms: Option<u64>,
    pub avg_bps: u64,
    pub peak_bps: u64,
    pub bytes: u64,
    pub elapsed_ms: u64,
    pub canceled: bool,
}

fn non_empty_str(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_cloudflare_meta(v: &serde_json::Value, latency_ms: u64) -> Option<IpInfo> {
    let ip = non_empty_str(v.get("clientIp"))?;
    let asn = v
        .get("asn")
        .and_then(|n| {
            n.as_u64()
                .map(|x| format!("AS{x}"))
                .or_else(|| n.as_str().map(str::to_string))
        })
        .filter(|s| !s.is_empty());
    Some(IpInfo {
        ip,
        country: non_empty_str(v.get("country")),
        region: non_empty_str(v.get("region")),
        city: non_empty_str(v.get("city")),
        isp: non_empty_str(v.get("asOrganization")),
        asn,
        latency_ms,
        source: "cloudflare".into(),
    })
}

fn parse_ipsb_json(v: &serde_json::Value, latency_ms: u64) -> Option<IpInfo> {
    let ip = non_empty_str(v.get("ip"))?;
    let asn = v
        .get("asn")
        .and_then(|n| n.as_u64())
        .map(|x| format!("AS{x}"));
    Some(IpInfo {
        ip,
        country: non_empty_str(v.get("country")),
        region: non_empty_str(v.get("region")),
        city: non_empty_str(v.get("city")),
        isp: non_empty_str(v.get("isp")).or_else(|| non_empty_str(v.get("organization"))),
        asn,
        latency_ms,
        source: "ip.sb".into(),
    })
}

fn parse_ipinfo_json(v: &serde_json::Value, latency_ms: u64) -> Option<IpInfo> {
    let ip = non_empty_str(v.get("ip"))?;
    let org = non_empty_str(v.get("org"));
    let (asn, isp) = match org {
        Some(ref o) if o.starts_with("AS") => {
            if let Some((a, rest)) = o.split_once(' ') {
                (Some(a.to_string()), Some(rest.trim().to_string()))
            } else {
                (Some(o.clone()), None)
            }
        }
        other => (None, other),
    };
    Some(IpInfo {
        ip,
        country: non_empty_str(v.get("country")),
        region: non_empty_str(v.get("region")),
        city: non_empty_str(v.get("city")),
        isp,
        asn,
        latency_ms,
        source: "ipinfo.io".into(),
    })
}

fn parse_ipip_json(v: &serde_json::Value, latency_ms: u64) -> Option<IpInfo> {
    let data = v.get("data")?;
    let ip = non_empty_str(data.get("ip"))?;
    let loc = data.get("location").and_then(|l| l.as_array());
    let get_loc = |idx: usize| -> Option<String> {
        loc.and_then(|arr| arr.get(idx))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(IpInfo {
        ip,
        country: get_loc(0),
        region: get_loc(1),
        city: get_loc(2),
        isp: get_loc(4).or_else(|| get_loc(3)),
        asn: None,
        latency_ms,
        source: "ipip.net".into(),
    })
}

#[derive(Clone, Copy)]
enum IpProvider {
    Cloudflare,
    IpSb,
    IpInfo,
    IpipNet,
}

impl IpProvider {
    fn url(self) -> &'static str {
        match self {
            Self::Cloudflare => "https://speed.cloudflare.com/meta",
            Self::IpSb => "https://api.ip.sb/geoip",
            Self::IpInfo => "https://ipinfo.io/json",
            Self::IpipNet => "https://myip.ipip.net/json",
        }
    }

    fn parse(self, v: &serde_json::Value, latency_ms: u64) -> Option<IpInfo> {
        match self {
            Self::Cloudflare => parse_cloudflare_meta(v, latency_ms),
            Self::IpSb => parse_ipsb_json(v, latency_ms),
            Self::IpInfo => parse_ipinfo_json(v, latency_ms),
            Self::IpipNet => parse_ipip_json(v, latency_ms),
        }
    }
}

/// 并发查询多个免费公开 IP 识别接口，返回最先成功的出口 IP 与归属信息。
pub async fn detect_ip(proxy: Option<&str>) -> Res<IpInfo> {
    let client = crate::build_client(proxy)?;
    let providers = [
        IpProvider::Cloudflare,
        IpProvider::IpSb,
        IpProvider::IpInfo,
        IpProvider::IpipNet,
    ];
    let mut set = JoinSet::new();
    for p in providers {
        let cl = client.clone();
        set.spawn(async move {
            let t0 = Instant::now();
            let res = tokio::time::timeout(Duration::from_secs(6), async {
                let resp = cl.get(p.url()).send().await?;
                if !resp.status().is_success() {
                    return Ok::<Option<IpInfo>, reqwest::Error>(None);
                }
                let text = resp.text().await?;
                let ms = t0.elapsed().as_millis() as u64;
                let parsed = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| p.parse(&v, ms));
                Ok(parsed)
            })
            .await;
            match res {
                Ok(Ok(Some(info))) => Some(info),
                _ => None,
            }
        });
    }
    while let Some(joined) = set.join_next().await {
        if let Ok(Some(info)) = joined {
            set.abort_all();
            return Ok(info);
        }
    }
    Err(DownloadError::new(
        ErrorKind::NetworkConnectionReset,
        "all public IP providers failed or timed out",
    ))
}

/// 根据延迟样本计算中位数延迟与平均抖动（ms）。
pub fn compute_latency_stats(samples: &[u64]) -> (Option<u64>, Option<u64>) {
    if samples.is_empty() {
        return (None, None);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let median = if sorted.len() % 2 == 1 {
        sorted[sorted.len() / 2]
    } else {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2
    };
    let jitter = if samples.len() >= 2 {
        let mut diffs = 0u64;
        for w in samples.windows(2) {
            diffs += w[0].abs_diff(w[1]);
        }
        Some(diffs / (samples.len() as u64 - 1))
    } else {
        Some(0)
    };
    (Some(median), jitter)
}

#[derive(Clone, Copy)]
struct SpeedServer {
    label: &'static str,
    ping_url: &'static str,
    ping_head: bool,
    download_url: &'static str,
}

const CLOUDFLARE_SERVER: SpeedServer = SpeedServer {
    label: "Cloudflare Global",
    ping_url: "https://speed.cloudflare.com/__down?bytes=0",
    ping_head: false,
    download_url: "https://speed.cloudflare.com/__down?bytes=26214400",
};

const CACHEFLY_SERVER: SpeedServer = SpeedServer {
    label: "CacheFly CDN",
    ping_url: "https://cachefly.cachefly.net/10mb.test",
    ping_head: true,
    download_url: "https://cachefly.cachefly.net/10mb.test",
};

async fn probe_ping_samples(
    client: &reqwest::Client,
    srv: SpeedServer,
    cancel: &CancellationToken,
) -> Option<(u64, u64)> {
    let mut samples = Vec::with_capacity(3);
    for _ in 0..3 {
        if cancel.is_cancelled() {
            return None;
        }
        let t0 = Instant::now();
        let fut = async {
            let req = if srv.ping_head {
                client.head(srv.ping_url)
            } else {
                client.get(srv.ping_url)
            };
            let resp = req.send().await?;
            let ok = resp.status().is_success();
            if ok && !srv.ping_head {
                let _ = resp.bytes().await;
            }
            Ok::<bool, reqwest::Error>(ok)
        };
        let ok = tokio::select! {
            _ = cancel.cancelled() => return None,
            r = tokio::time::timeout(Duration::from_secs(4), fut) => {
                matches!(r, Ok(Ok(true)))
            }
        };
        if ok {
            samples.push(t0.elapsed().as_millis() as u64);
        }
    }
    let (lat, jit) = compute_latency_stats(&samples);
    Some((lat?, jit.unwrap_or(0)))
}

struct MeasureSpec<'a> {
    download_url: &'a str,
    server_label: &'a str,
    latency_ms: Option<u64>,
    jitter_ms: Option<u64>,
    duration: Duration,
    workers: usize,
    max_bytes: u64,
}

async fn measure_download_from_url(
    worker_client: &reqwest::Client,
    spec: MeasureSpec<'_>,
    cancel: CancellationToken,
    on_progress: Arc<dyn Fn(SpeedProgress) + Send + Sync>,
) -> Res<SpeedTestResult> {
    let bytes_counter = Arc::new(AtomicU64::new(0));
    let worker_cancel = cancel.child_token();
    let mut join = JoinSet::new();

    for _ in 0..spec.workers.max(1) {
        let cl = worker_client.clone();
        let url = spec.download_url.to_string();
        let counter = bytes_counter.clone();
        let tok = worker_cancel.clone();
        let max_bytes = spec.max_bytes;
        join.spawn(async move {
            loop {
                if tok.is_cancelled() || counter.load(Ordering::Relaxed) >= max_bytes {
                    break;
                }
                let resp = tokio::select! {
                    _ = tok.cancelled() => break,
                    r = cl.get(&url).send() => match r {
                        Ok(resp) if resp.status().is_success() => resp,
                        _ => break,
                    }
                };
                let mut resp = resp;
                loop {
                    let chunk = tokio::select! {
                        _ = tok.cancelled() => break,
                        c = resp.chunk() => match c {
                            Ok(Some(b)) => b,
                            _ => break,
                        }
                    };
                    let total = counter.fetch_add(chunk.len() as u64, Ordering::Relaxed)
                        + chunk.len() as u64;
                    if total >= max_bytes {
                        tok.cancel();
                        break;
                    }
                }
            }
        });
    }

    let start = Instant::now();
    let mut last_tick = start;
    let mut last_bytes = 0u64;
    let mut peak_bps = 0u64;
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let deadline = tokio::time::sleep(spec.duration);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = &mut deadline => break,
            _ = ticker.tick() => {
                let now = Instant::now();
                let cur_bytes = bytes_counter.load(Ordering::Relaxed);
                let dt = now.duration_since(last_tick).as_secs_f64();
                let elapsed = now.duration_since(start);
                let current_bps = if dt > 0.05 {
                    ((cur_bytes.saturating_sub(last_bytes)) as f64 / dt) as u64
                } else {
                    0
                };
                let elapsed_secs = elapsed.as_secs_f64().max(0.05);
                let avg_bps = (cur_bytes as f64 / elapsed_secs) as u64;
                if current_bps > peak_bps {
                    peak_bps = current_bps;
                }
                last_tick = now;
                last_bytes = cur_bytes;

                on_progress(SpeedProgress {
                    phase: "download".into(),
                    elapsed_ms: elapsed.as_millis() as u64,
                    bytes: cur_bytes,
                    current_bps,
                    avg_bps,
                    peak_bps,
                    latency_ms: spec.latency_ms,
                    jitter_ms: spec.jitter_ms,
                    server: spec.server_label.into(),
                });

                if worker_cancel.is_cancelled() || join.is_empty() {
                    break;
                }
            }
        }
    }

    worker_cancel.cancel();
    while join.join_next().await.is_some() {}

    let elapsed_ms = start.elapsed().as_millis().max(1) as u64;
    let total_bytes = bytes_counter.load(Ordering::Relaxed);
    let avg_bps = total_bytes.saturating_mul(1000) / elapsed_ms;
    peak_bps = peak_bps.max(avg_bps);
    let canceled = cancel.is_cancelled();

    if !canceled && total_bytes == 0 {
        return Err(DownloadError::new(
            ErrorKind::NetworkConnectionReset,
            "speed test downloaded 0 bytes",
        ));
    }

    on_progress(SpeedProgress {
        phase: "done".into(),
        elapsed_ms,
        bytes: total_bytes,
        current_bps: avg_bps,
        avg_bps,
        peak_bps,
        latency_ms: spec.latency_ms,
        jitter_ms: spec.jitter_ms,
        server: spec.server_label.into(),
    });

    Ok(SpeedTestResult {
        server: spec.server_label.into(),
        latency_ms: spec.latency_ms,
        jitter_ms: spec.jitter_ms,
        avg_bps,
        peak_bps,
        bytes: total_bytes,
        elapsed_ms,
        canceled,
    })
}

/// 运行网络延迟 + 多线程流式下载测速（数据仅在内存丢弃，不写磁盘，严格遵循 `proxy`）。
pub async fn run_speed_test(
    proxy: Option<&str>,
    server_id: &str,
    cancel: CancellationToken,
    on_progress: impl Fn(SpeedProgress) + Send + Sync + 'static,
) -> Res<SpeedTestResult> {
    let ping_client = crate::build_client(proxy)?;
    let worker_client = crate::build_pooled_worker_client(proxy, 4)?;
    let progress: Arc<dyn Fn(SpeedProgress) + Send + Sync> = Arc::new(on_progress);

    let (srv, latency_ms, jitter_ms) = match server_id {
        "cachefly" => {
            progress(SpeedProgress {
                phase: "ping".into(),
                elapsed_ms: 0,
                bytes: 0,
                current_bps: 0,
                avg_bps: 0,
                peak_bps: 0,
                latency_ms: None,
                jitter_ms: None,
                server: CACHEFLY_SERVER.label.into(),
            });
            let (lat, jit) = probe_ping_samples(&ping_client, CACHEFLY_SERVER, &cancel)
                .await
                .map(|(l, j)| (Some(l), Some(j)))
                .unwrap_or((None, None));
            (CACHEFLY_SERVER, lat, jit)
        }
        "cloudflare" => {
            progress(SpeedProgress {
                phase: "ping".into(),
                elapsed_ms: 0,
                bytes: 0,
                current_bps: 0,
                avg_bps: 0,
                peak_bps: 0,
                latency_ms: None,
                jitter_ms: None,
                server: CLOUDFLARE_SERVER.label.into(),
            });
            let (lat, jit) = probe_ping_samples(&ping_client, CLOUDFLARE_SERVER, &cancel)
                .await
                .map(|(l, j)| (Some(l), Some(j)))
                .unwrap_or((None, None));
            (CLOUDFLARE_SERVER, lat, jit)
        }
        _ => {
            progress(SpeedProgress {
                phase: "ping".into(),
                elapsed_ms: 0,
                bytes: 0,
                current_bps: 0,
                avg_bps: 0,
                peak_bps: 0,
                latency_ms: None,
                jitter_ms: None,
                server: CLOUDFLARE_SERVER.label.into(),
            });
            if let Some((l, j)) = probe_ping_samples(&ping_client, CLOUDFLARE_SERVER, &cancel).await
            {
                (CLOUDFLARE_SERVER, Some(l), Some(j))
            } else if cancel.is_cancelled() {
                (CLOUDFLARE_SERVER, None, None)
            } else {
                let (l, j) = probe_ping_samples(&ping_client, CACHEFLY_SERVER, &cancel)
                    .await
                    .map(|(l, j)| (Some(l), Some(j)))
                    .unwrap_or((None, None));
                (CACHEFLY_SERVER, l, j)
            }
        }
    };

    if cancel.is_cancelled() {
        return Ok(SpeedTestResult {
            server: srv.label.into(),
            latency_ms,
            jitter_ms,
            avg_bps: 0,
            peak_bps: 0,
            bytes: 0,
            elapsed_ms: 0,
            canceled: true,
        });
    }

    progress(SpeedProgress {
        phase: "download".into(),
        elapsed_ms: 0,
        bytes: 0,
        current_bps: 0,
        avg_bps: 0,
        peak_bps: 0,
        latency_ms,
        jitter_ms,
        server: srv.label.into(),
    });

    measure_download_from_url(
        &worker_client,
        MeasureSpec {
            download_url: srv.download_url,
            server_label: srv.label,
            latency_ms,
            jitter_ms,
            duration: Duration::from_secs(8),
            workers: 4,
            max_bytes: 64 * 1024 * 1024,
        },
        cancel,
        progress,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn ip_parsers_extract_fields_across_providers() {
        let cf: serde_json::Value = serde_json::from_str(
            r#"{"clientIp":"1.2.3.4","country":"JP","region":"Tokyo","city":"Tokyo","asOrganization":"Cloudflare, Inc.","asn":13335}"#,
        )
        .unwrap();
        let i1 = parse_cloudflare_meta(&cf, 42).unwrap();
        assert_eq!(i1.ip, "1.2.3.4");
        assert_eq!(i1.country.as_deref(), Some("JP"));
        assert_eq!(i1.asn.as_deref(), Some("AS13335"));
        assert_eq!(i1.isp.as_deref(), Some("Cloudflare, Inc."));

        let sb: serde_json::Value = serde_json::from_str(
            r#"{"ip":"5.6.7.8","country":"Singapore","region":"Central","city":"Singapore","isp":"DigitalOcean","asn":14061}"#,
        )
        .unwrap();
        let i2 = parse_ipsb_json(&sb, 30).unwrap();
        assert_eq!(i2.ip, "5.6.7.8");
        assert_eq!(i2.isp.as_deref(), Some("DigitalOcean"));

        let info: serde_json::Value = serde_json::from_str(
            r#"{"ip":"9.9.9.9","city":"Zurich","region":"Zurich","country":"CH","org":"AS19281 Quad9"}"#,
        )
        .unwrap();
        let i3 = parse_ipinfo_json(&info, 55).unwrap();
        assert_eq!(i3.asn.as_deref(), Some("AS19281"));
        assert_eq!(i3.isp.as_deref(), Some("Quad9"));

        let ipip: serde_json::Value = serde_json::from_str(
            r#"{"ret":"ok","data":{"ip":"114.114.114.114","location":["中国","广东","深圳","","电信"]}}"#,
        )
        .unwrap();
        let i4 = parse_ipip_json(&ipip, 18).unwrap();
        assert_eq!(i4.ip, "114.114.114.114");
        assert_eq!(i4.country.as_deref(), Some("中国"));
        assert_eq!(i4.city.as_deref(), Some("深圳"));
        assert_eq!(i4.isp.as_deref(), Some("电信"));

        assert!(parse_cloudflare_meta(&serde_json::json!({}), 10).is_none());
    }

    #[test]
    fn latency_stats_computes_median_and_jitter() {
        assert_eq!(compute_latency_stats(&[]), (None, None));
        assert_eq!(compute_latency_stats(&[40]), (Some(40), Some(0)));
        assert_eq!(compute_latency_stats(&[20, 50, 30]), (Some(30), Some(25)));
    }

    #[tokio::test]
    async fn measure_download_from_local_server_reports_speed_and_cancels() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let chunk = vec![b'A'; 64 * 1024];
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let body = chunk.clone();
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 1024];
                    let _ = sock.read(&mut req_buf).await;
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if sock.write_all(hdr.as_bytes()).await.is_ok() {
                        let _ = sock.write_all(&body).await;
                    }
                });
            }
        });

        let client = crate::build_pooled_worker_client(Some("direct"), 2).unwrap();
        let url = format!("http://{addr}/blob");
        let events = Arc::new(std::sync::Mutex::new(Vec::<SpeedProgress>::new()));
        let ev_clone = events.clone();
        let res = measure_download_from_url(
            &client,
            MeasureSpec {
                download_url: &url,
                server_label: "local-test",
                latency_ms: Some(5),
                jitter_ms: Some(1),
                duration: Duration::from_secs(2),
                workers: 2,
                max_bytes: 256 * 1024,
            },
            CancellationToken::new(),
            Arc::new(move |p| ev_clone.lock().unwrap().push(p)),
        )
        .await
        .unwrap();

        assert!(!res.canceled);
        assert!(res.bytes >= 256 * 1024);
        assert!(res.avg_bps > 0);
        assert!(events.lock().unwrap().iter().any(|e| e.phase == "done"));
        server.abort();
    }
}
