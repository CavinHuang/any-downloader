use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use dl_engine::dash::{expand_template, parse_iso8601_duration, parse_mpd, strip_url_fragment};
use dl_engine::{DownloadOptions, Engine};
use dl_error::ErrorKind;
use dl_model::{CreateTask, TaskKind, TaskStatus};

fn serve_routes(routes: HashMap<&'static str, (&'static str, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = Arc::new(Mutex::new(routes));
    std::thread::spawn(move || {
        while let Ok((mut sock, _)) = listener.accept() {
            let routes = routes.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let path = first_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let found = routes.lock().unwrap().get(path.as_str()).cloned();
                match found {
                    Some((ctype, body)) => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = sock.write_all(head.as_bytes());
                        let _ = sock.write_all(&body);
                    }
                    None => {
                        let _ =
                            sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    }
                }
            });
        }
    });
    format!("http://{addr}")
}

#[test]
fn iso8601_duration_parsing() {
    assert_eq!(parse_iso8601_duration("PT1H2M3.5S"), Some(3723.5));
    assert_eq!(parse_iso8601_duration("PT45S"), Some(45.0));
    assert_eq!(parse_iso8601_duration("PT2M"), Some(120.0));
    assert_eq!(parse_iso8601_duration("invalid"), None);
}

#[test]
fn template_placeholder_expansion() {
    assert_eq!(
        expand_template(
            "v/$RepresentationID$/seg-$Number%04d$-$$.m4s",
            "1080p",
            5000,
            7,
            140
        ),
        "v/1080p/seg-0007-$.m4s"
    );
    assert_eq!(
        expand_template("chunk-$Time$-bw$Bandwidth$.m4s", "a1", 128000, 1, 90000),
        "chunk-90000-bw128000.m4s"
    );
}

#[test]
fn parse_mpd_variants_timeline_list_and_guards() {
    let timeline_mpd = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD type="static" mediaPresentationDuration="PT10S">
  <Period>
    <AdaptationSet mimeType="video/mp4">
      <SegmentTemplate initialization="v/$RepresentationID$/init.mp4" media="v/$RepresentationID$/seg-$Number%03d$.m4s" timescale="1000" startNumber="1">
        <SegmentTimeline>
          <S t="0" d="2000" r="2"/>
          <S d="4000"/>
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="720" bandwidth="1500000" width="1280" height="720"/>
      <Representation id="1080" bandwidth="3000000" width="1920" height="1080"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
    let parsed = parse_mpd(timeline_mpd, "https://cdn.example.com/live/stream.mpd").unwrap();
    assert_eq!(parsed.duration_sec, Some(10.0));
    assert_eq!(parsed.video_reps.len(), 2);
    let r1080 = &parsed.video_reps[1];
    assert_eq!(
        r1080.init.as_ref().unwrap().uri,
        "https://cdn.example.com/live/v/1080/init.mp4"
    );
    assert_eq!(r1080.segments.len(), 4);
    assert_eq!(
        r1080.segments[0].uri,
        "https://cdn.example.com/live/v/1080/seg-001.m4s"
    );
    assert_eq!(
        r1080.segments[3].uri,
        "https://cdn.example.com/live/v/1080/seg-004.m4s"
    );

    let list_mpd = r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4">
      <Representation id="v1" bandwidth="800000" width="640" height="360">
        <SegmentList>
          <Initialization sourceURL="init.mp4"/>
          <SegmentURL media="part1.m4s"/>
          <SegmentURL media="part2.m4s" mediaRange="100-199"/>
        </SegmentList>
      </Representation>
    </AdaptationSet></Period></MPD>"#;
    let list_parsed = parse_mpd(list_mpd, "https://example.com/video/manifest.mpd").unwrap();
    assert_eq!(list_parsed.video_reps[0].segments.len(), 2);
    assert_eq!(
        list_parsed.video_reps[0].segments[1].range,
        Some((100, 100))
    );

    let base_mpd = r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4">
      <Representation id="full" bandwidth="2000000" width="1920" height="1080">
        <BaseURL>movie.mp4</BaseURL>
      </Representation>
    </AdaptationSet></Period></MPD>"#;
    let base_parsed = parse_mpd(base_mpd, "https://example.com/stream.mpd").unwrap();
    assert_eq!(
        base_parsed.video_reps[0].segments[0].uri,
        "https://example.com/movie.mp4"
    );

    let live_mpd = r#"<MPD type="dynamic"><Period/></MPD>"#;
    let live_err = parse_mpd(live_mpd, "https://example.com/live.mpd").unwrap_err();
    let live_dbg = format!("{live_err:?}");
    assert!(live_dbg.contains("LiveUnsupported"), "{live_dbg}");

    let drm_mpd = r#"<MPD type="static"><Period>
      <AdaptationSet mimeType="video/mp4">
        <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
        <Representation id="v1" bandwidth="1000" width="640" height="360"><BaseURL>enc.mp4</BaseURL></Representation>
      </AdaptationSet>
    </Period></MPD>"#;
    let drm_parsed = parse_mpd(drm_mpd, "https://example.com/drm.mpd").unwrap();
    assert!(drm_parsed.video_reps[0].has_drm);

    assert_eq!(
        strip_url_fragment("https://x/a.mpd#adl_rep=1080p"),
        ("https://x/a.mpd".into(), Some("1080p".into()))
    );
}

#[tokio::test]
async fn dash_end_to_end_download_and_variant_selection() {
    let mpd = r#"<?xml version="1.0" encoding="utf-8"?>
<MPD type="static" mediaPresentationDuration="PT4S">
  <Period>
    <AdaptationSet mimeType="video/mp4">
      <SegmentTemplate initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg-$Number$.m4s" timescale="1" duration="2" startNumber="1"/>
      <Representation id="720" bandwidth="1000000" width="1280" height="720"/>
      <Representation id="1080" bandwidth="2500000" width="1920" height="1080"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
    let mut routes = HashMap::new();
    routes.insert(
        "/stream.mpd",
        ("application/dash+xml", mpd.as_bytes().to_vec()),
    );
    routes.insert("/1080/init.mp4", ("video/mp4", b"INIT1080-".to_vec()));
    routes.insert("/1080/seg-1.m4s", ("video/mp4", b"SEG1-".to_vec()));
    routes.insert("/1080/seg-2.m4s", ("video/mp4", b"SEG2!".to_vec()));
    routes.insert("/720/init.mp4", ("video/mp4", b"INIT720-".to_vec()));
    routes.insert("/720/seg-1.m4s", ("video/mp4", b"LOW1-".to_vec()));
    routes.insert("/720/seg-2.m4s", ("video/mp4", b"LOW2!".to_vec()));
    let base = serve_routes(routes);

    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = Engine::open(&db.path().join("data.db")).await.unwrap();

    // 1. 默认自动挑最高清晰度 1080p，并写回 variants_json
    let t = engine
        .create_task(CreateTask {
            filename: Some("demo.mp4".into()),
            dir: Some(out.path().display().to_string()),
            ..CreateTask::of(format!("{base}/stream.mpd"))
        })
        .await
        .unwrap();
    assert_eq!(t.kind, TaskKind::Dash);
    let done = engine
        .download(
            &t.id,
            DownloadOptions {
                dir: out.path().to_path_buf(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(done.status, TaskStatus::Done);
    assert!(done
        .variants_json
        .as_deref()
        .unwrap_or("")
        .contains("1920x1080"));
    let got = std::fs::read(out.path().join("demo.mp4")).unwrap();
    assert_eq!(got, b"INIT1080-SEG1-SEG2!");

    // 2. 指定 #adl_rep=720 下载指定清晰度
    let t2 = engine
        .create_task(CreateTask {
            filename: Some("demo-720.mp4".into()),
            dir: Some(out.path().display().to_string()),
            ..CreateTask::of(format!("{base}/stream.mpd#adl_rep=720"))
        })
        .await
        .unwrap();
    let done2 = engine
        .download(
            &t2.id,
            DownloadOptions {
                dir: out.path().to_path_buf(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(done2.status, TaskStatus::Done);
    let got2 = std::fs::read(out.path().join("demo-720.mp4")).unwrap();
    assert_eq!(got2, b"INIT720-LOW1-LOW2!");
}

#[tokio::test]
async fn dash_drm_returns_drm_unsupported() {
    let mpd = r#"<MPD type="static"><Period><AdaptationSet mimeType="video/mp4">
      <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
      <Representation id="v1" bandwidth="1000" width="640" height="360"><BaseURL>enc.mp4</BaseURL></Representation>
    </AdaptationSet></Period></MPD>"#;
    let mut routes = HashMap::new();
    routes.insert(
        "/drm.mpd",
        ("application/dash+xml", mpd.as_bytes().to_vec()),
    );
    let base = serve_routes(routes);

    let db = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let engine = Engine::open(&db.path().join("data.db")).await.unwrap();
    let t = engine
        .create_task(CreateTask::of(format!("{base}/drm.mpd")))
        .await
        .unwrap();
    let err = engine
        .download(
            &t.id,
            DownloadOptions {
                dir: out.path().to_path_buf(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::DrmUnsupported);
}
