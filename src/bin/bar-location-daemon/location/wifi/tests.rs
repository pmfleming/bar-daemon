use super::*;
use http::LookupResult;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn filters_ambiguous_opted_out_stale_invalid_and_weak_access_points() {
    let valid = "00:11:22:33:44:55";
    assert!(point(valid, b"network", 80, 102, 101_500, 103_000).is_some());
    for mac in [
        "7a:11:22:29:92:b4",
        "7a:11:22:39:92:b4",
        "7a:11:22:49:92:b4",
        "02:11:22:33:44:55",
        "01:11:22:33:44:55",
        "ff:ff:ff:ff:ff:ff",
        "00:00:00:00:00:00",
        "00:11:22:33:44",
        "00:11:22:33:44:gg",
        "0:11:22:33:44:55",
        "001122334455",
    ] {
        assert!(
            point(mac, b"network", 80, 102, 101_500, 103_000).is_none(),
            "{mac}"
        );
    }
    for ssid in [b"".as_slice(), b"private_nomap", &[b'a'; 33]] {
        assert!(point(valid, ssid, 80, 102, 101_500, 103_000).is_none());
    }
    for seen in [-1, 100, 101, 104] {
        assert!(point(valid, b"network", 80, seen, 101_500, 103_000).is_none());
    }
    assert!(point(valid, b"network", 80, 102, 101_500, 163_000).is_none());
    for strength in [0, 15, 101, 255] {
        assert!(point(valid, b"network", strength, 102, 101_500, 103_000).is_none());
    }
    assert_eq!(
        eligible_address("A0:BB:CC:DD:EE:FF").unwrap(),
        "a0:bb:cc:dd:ee:ff"
    );
    assert_eq!(
        point(valid, b"network", 50, 102, 101_500, 103_000)
            .unwrap()
            .signal_strength,
        -70
    );
}

fn good_response() -> serde_json::Value {
    serde_json::json!({"location": {"lat": 53.35, "lng": -6.26}, "accuracy": 120.0})
}

#[test]
fn validates_response_and_preserves_observation_time_not_reply_time() {
    let observed = wall_time() - 20.0;
    let started = observed - 1.0;
    let good = good_response();
    let fix = parse(&serde_json::to_vec(&good).unwrap(), observed, started).unwrap();
    assert_eq!(fix.timestamp, observed);
    assert_eq!(fix.source, "WiFi");
    for (key, value) in [
        ("fallback", serde_json::json!("ipf")),
        ("fallback", serde_json::json!("lacf")),
        ("fallback", serde_json::json!("")),
        ("error", serde_json::json!(false)),
        ("accuracy", serde_json::json!(0)),
        ("accuracy", serde_json::json!(25001)),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        assert!(parse(&serde_json::to_vec(&bad).unwrap(), observed, started).is_err());
    }
    for coords in [
        serde_json::json!({"lat":91,"lng":0}),
        serde_json::json!({"lat":0,"lng":181}),
        serde_json::json!({"lat":null,"lng":0}),
    ] {
        let mut bad = good.clone();
        bad["location"] = coords;
        assert!(parse(&serde_json::to_vec(&bad).unwrap(), observed, started).is_err());
    }
    assert!(parse(&serde_json::to_vec(&good).unwrap(), started - 1.0, started).is_err());
    assert!(
        parse(
            &serde_json::to_vec(&good).unwrap(),
            wall_time() - 301.0,
            wall_time() - 302.0
        )
        .is_err()
    );
    let error = parse(
        br#"{"location":"private identifiers must not be echoed"}"#,
        observed,
        started,
    )
    .err()
    .unwrap();
    assert!(!error.to_string().contains("private identifiers"));
}

fn sample_scan() -> Scan {
    let now = boottime_ms().unwrap();
    Scan {
        observed_at: wall_time(),
        points: ["00:11:22:33:44:55", "00:11:22:33:44:66"]
            .iter()
            .map(|mac| AccessPoint {
                mac_address: (*mac).into(),
                signal_strength: -65,
                age: 0,
                last_seen_ms: now,
            })
            .collect(),
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0 && bytes.len() + n <= 65536);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end])
                .unwrap()
                .to_ascii_lowercase();
            assert!(headers.starts_with("post / "));
            let length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn transport_sends_wifi_only_and_bounds_failures_cooldowns_and_payloads() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    for (headers, body, stall, expected) in [
        ("200 OK", good_response().to_string(), false, "fix"),
        (
            "404 Not Found",
            r#"{"error":{"code":404}}"#.into(),
            false,
            "not_recognised",
        ),
        (
            "429 Limited\r\nRetry-After: 7200",
            String::new(),
            false,
            "limit",
        ),
        (
            "302 Found\r\nLocation: http://127.0.0.1:9/",
            String::new(),
            false,
            "error",
        ),
        ("500 Failed", String::new(), false, "error"),
        ("200 OK", "not json".into(), false, "error"),
        ("200 OK", " ".repeat(65537), false, "error"),
        ("200 OK", String::new(), true, "error"),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            assert_eq!(request["considerIp"], false);
            assert_eq!(
                request["fallbacks"],
                serde_json::json!({"ipf":false, "lacf":false})
            );
            assert!(request.get("cellTowers").is_none());
            let points = request["wifiAccessPoints"].as_array().unwrap();
            assert_eq!(points.len(), 2);
            for point in points {
                assert_eq!(
                    point.as_object().unwrap().len(),
                    3,
                    "only address, strength and age are sent"
                );
                assert!(point["age"].as_u64().unwrap() <= 60_000);
                assert!(eligible_address(point["macAddress"].as_str().unwrap()).is_some());
            }
            if stall {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let _ = stream
                .write_all(
                    format!("HTTP/1.1 {headers}\r\nConnection: close\r\n\r\n{body}").as_bytes(),
                )
                .await;
        });
        let started = wall_time();
        let result = fetch(&client, &url, sample_scan(), started).await;
        match expected {
            "fix" => assert!(matches!(result, Ok(LookupResult::Fix(_)))),
            "not_recognised" => assert!(matches!(result, Ok(LookupResult::NotRecognised))),
            "limit" => {
                assert!(matches!(result, Ok(LookupResult::Limited(d)) if d.as_secs() == 7200))
            }
            _ => assert!(result.is_err()),
        }
        server.await.unwrap();
    }
    assert!(
        fetch(
            &http::client().unwrap(),
            "http://127.0.0.1:9/",
            sample_scan(),
            wall_time() - 1.0
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn insufficient_or_expired_observations_never_send_http() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let client = reqwest::Client::new();
    for scenario in ["one", "expired", "pre_attempt"] {
        let started = wall_time();
        let mut scan = sample_scan();
        match scenario {
            "one" => {
                scan.points.pop();
            }
            "expired" => {
                for p in &mut scan.points {
                    p.last_seen_ms -= 61_000;
                }
            }
            _ => scan.observed_at = started - 1.0,
        }
        assert!(fetch(&client, &url, scan, started).await.is_err());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
