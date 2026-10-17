use super::{policy::Fix, wall_time};
use anyhow::{Result, ensure};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::time::Duration;

pub(super) enum LookupResult {
    Fix(Fix),
    Limited(Duration),
    Unavailable,
}

pub(super) fn client() -> Result<Client> {
    Ok(Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .user_agent("Shelllist-location-timezone/3")
        .build()?)
}
pub(super) async fn lookup(client: &Client) -> LookupResult {
    match fetch(client, "https://ipapi.co/json/").await {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(error = %error, "IP location lookup failed");
            LookupResult::Unavailable
        }
    }
}
async fn fetch(client: &Client, url: &str) -> Result<LookupResult> {
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    match response_body(response, "ipapi.co").await? {
        Body::Bytes(bytes) => Ok(LookupResult::Fix(parse(&bytes, wall_time())?)),
        Body::Limited(delay) => Ok(LookupResult::Limited(delay)),
    }
}

pub(super) enum Body {
    Bytes(Vec<u8>),
    Limited(Duration),
}

pub(super) async fn response_body(mut response: reqwest::Response, provider: &str) -> Result<Body> {
    tracing::info!(
        provider,
        http_status = response.status().as_u16(),
        "Location response received"
    );
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        return Ok(Body::Limited(retry_after(
            response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok()),
            wall_time(),
        )));
    }
    ensure!(
        response.status().is_success(),
        "location lookup rejected: HTTP {}",
        response.status()
    );
    ensure!(
        response.content_length().unwrap_or(0) <= 65536,
        "location response too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        ensure!(
            bytes.len() + chunk.len() <= 65536,
            "location response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(Body::Bytes(bytes))
}
fn parse(bytes: &[u8], now: f64) -> Result<Fix> {
    #[derive(Deserialize)]
    struct Payload {
        latitude: f64,
        longitude: f64,
        timezone: String,
        country_code: String,
        #[serde(default)]
        error: bool,
    }
    let value: Payload = serde_json::from_slice(bytes).map_err(|error| {
        anyhow::anyhow!(
            "invalid IP response JSON at line {}, column {}",
            error.line(),
            error.column()
        )
    })?;
    ensure!(
        !value.error
            && !value.timezone.is_empty()
            && value.timezone.len() <= 100
            && value.country_code.len() == 2
            && value.country_code.bytes().all(|c| c.is_ascii_alphabetic()),
        "invalid IP location"
    );
    let fix = Fix {
        latitude: value.latitude,
        longitude: value.longitude,
        timezone: value.timezone,
        country_code: value.country_code.to_ascii_uppercase(),
        source: "IP".into(),
        accuracy: 0.0,
        timestamp: now,
    };
    ensure!(fix.coordinates_valid(), "invalid IP coordinates");
    Ok(fix)
}
fn retry_after(value: Option<&str>, now: f64) -> Duration {
    let seconds = value
        .and_then(|v| {
            v.parse::<u64>().ok().map(|v| v as f64).or_else(|| {
                chrono::DateTime::parse_from_rfc2822(v)
                    .ok()
                    .map(|d| d.timestamp() as f64 - now)
            })
        })
        .unwrap_or(3600.0);
    Duration::from_secs_f64(seconds.clamp(60.0, 365.0 * 86400.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn transport_bounds_redirects_stream_size_failures_and_deadlines() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        for (headers, body, stall, expected) in [
            ("200 OK", r#"{"latitude":53.34,"longitude":-6.27,"timezone":"Europe/Dublin","country_code":"IE"}"#.into(), false, "fix"),
            ("302 Found\r\nLocation: http://127.0.0.1:9/", String::new(), false, "error"),
            ("500 Failed", String::new(), false, "error"),
            ("429 Limited\r\nRetry-After: 7200", String::new(), false, "limit"),
            ("200 OK", "not json".into(), false, "error"),
            ("200 OK", " ".repeat(65537), false, "error"),
            ("200 OK", String::new(), true, "error"),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = stream.read(&mut [0; 4096]).await.unwrap();
                if stall { tokio::time::sleep(Duration::from_millis(250)).await; }
                // No Content-Length: exercise the streaming bound, not only headers.
                let _ = stream.write_all(format!("HTTP/1.1 {headers}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await;
            });
            let result = fetch(&client, &url).await;
            match expected {
                "fix" => assert!(matches!(result, Ok(LookupResult::Fix(_)))),
                "limit" => assert!(matches!(result, Ok(LookupResult::Limited(d)) if d.as_secs() == 7200)),
                _ => assert!(result.is_err()),
            }
            server.await.unwrap();
        }
        assert!(
            fetch(&super::client().unwrap(), "http://127.0.0.1:9/")
                .await
                .is_err(),
            "production permits HTTPS only"
        );
    }

    #[test]
    fn bounded_ip_payload_and_cooldown_parsing() {
        let good = br#"{"latitude":53.34,"longitude":-6.27,"timezone":"Europe/Dublin","country_code":"ie"}"#;
        assert_eq!(parse(good, 100.0).unwrap().country_code, "IE");
        for field in ["latitude", "longitude", "timezone", "country_code"] {
            let mut value: serde_json::Value = serde_json::from_slice(good).unwrap();
            value[field] = serde_json::Value::Null;
            assert!(parse(&serde_json::to_vec(&value).unwrap(), 100.0).is_err());
        }
        assert_eq!(retry_after(None, 0.0).as_secs(), 3600);
        assert_eq!(retry_after(Some("garbage"), 0.0).as_secs(), 3600);
        assert_eq!(retry_after(Some("0"), 0.0).as_secs(), 60);
        assert_eq!(retry_after(Some("7200"), 0.0).as_secs(), 7200);
        assert_eq!(
            retry_after(Some("Thu, 01 Jan 1970 01:00:00 GMT"), 100.0).as_secs(),
            3500
        );
    }
}
