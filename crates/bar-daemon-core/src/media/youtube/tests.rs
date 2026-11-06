use super::{
    JSON_LIMIT, bounded_get, download_artwork, image_signature, oembed_url, parse, public_ip, text,
    thumbnail_url,
};
use reqwest::{Client, Url};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[test]
fn ids_endpoints_payloads_and_thumbnails_are_constrained() {
    let id = "RQzh-xnLRlM";
    for raw in [
        format!("https://youtube.com/watch?v={id}&token=secret#private"),
        format!("https://youtu.be/{id}?si=private"),
        format!("https://www.youtube.com/shorts/{id}"),
        format!("https://youtube-nocookie.com/embed/{id}"),
        format!("https://www.youtube.com/live/{id}"),
    ] {
        let video = crate::media::source::youtube_video_id(&raw).unwrap();
        assert_eq!(video, id);
        let endpoint = oembed_url(&video).unwrap();
        assert_eq!(endpoint.host_str(), Some("www.youtube.com"));
        let query: Vec<_> = endpoint.query_pairs().collect();
        assert_eq!(query[0].1, format!("https://www.youtube.com/watch?v={id}"));
        assert!(!endpoint.as_str().contains("secret"));
        assert!(!endpoint.as_str().contains("private"));
    }
    for raw in [
        "https://youtube.com/watch?v=RQzh-xnLRlM&v=RQzh-xnLRlM",
        "https://youtube.com/@channel",
        "https://youtube.com/watch?v=bad",
        "https://youtube.com.evil.test/watch?v=RQzh-xnLRlM",
        "https://user:password@youtube.com/watch?v=RQzh-xnLRlM",
        "http://127.0.0.1/watch?v=RQzh-xnLRlM",
        "https://youtube.com:444/watch?v=RQzh-xnLRlM",
    ] {
        assert!(
            crate::media::source::youtube_video_id(raw).is_none(),
            "{raw}"
        );
    }
    let (data, thumbnail) = parse(br#"{"type":"video","title":"Video","author_name":"Channel","thumbnail_url":"https://i.ytimg.com/vi/RQzh-xnLRlM/hqdefault.jpg","html":"<script>ignored</script>"}"#, id).unwrap();
    assert_eq!(data.title, "Video");
    assert_eq!(data.artist, "Channel");
    assert!(thumbnail.is_some());
    for url in [
        "http://i.ytimg.com/vi/RQzh-xnLRlM/hqdefault.jpg",
        "https://127.0.0.1/vi/RQzh-xnLRlM/hqdefault.jpg",
        "https://i.ytimg.com.evil.test/vi/RQzh-xnLRlM/hqdefault.jpg",
        "https://i.ytimg.com/vi/DIFFERENTID/hqdefault.jpg",
        "https://i.ytimg.com/vi/RQzh-xnLRlM/hqdefault.jpg?token=secret",
        "https://i.ytimg.com/vi/RQzh-xnLRlM/redirect",
        "https://i.ytimg.com/vi/RQzh-xnLRlM/default.webp",
        "https://i.ytimg.com/vi_webp/RQzh-xnLRlM/default.jpg",
        "https://i.ytimg.com/vi/RQzh-xnLRlM/default.jpg/extra",
        "https://i.ytimg.com/vi/RQzh-xnLRlM/default.jpg/",
        "https://i.ytimg.com/vi/RQzh-xnLRlM/default.jpg.png",
        "https://user:pass@i.ytimg.com/vi/RQzh-xnLRlM/hqdefault.jpg",
    ] {
        assert!(thumbnail_url(url, id).is_none(), "{url}");
    }
    for (directory, extension) in [("vi", "jpg"), ("vi_webp", "webp")] {
        for size in [
            "default",
            "mqdefault",
            "hqdefault",
            "sddefault",
            "maxresdefault",
        ] {
            let raw = format!("https://i.ytimg.com/{directory}/{id}/{size}.{extension}");
            assert_eq!(
                thumbnail_url(&raw, id).as_ref().map(Url::as_str),
                Some(raw.as_str())
            );
        }
    }
    assert!(parse(br#"{"type":"rich","title":"Wrong"}"#, id).is_err());
    assert!(parse(b"not JSON", id).is_err());
    assert!(text("bad\ntext").is_empty());
    assert!(text(&"x".repeat(4097)).is_empty());
    assert!(image_signature(b"\xff\xd8\xffmore"));
    assert!(!image_signature(b"<html>error</html>"));
}

#[test]
fn private_and_special_dns_addresses_are_never_used() {
    for ip in [
        "0.0.0.0",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "192.168.1.1",
        "192.0.0.8",
        "192.0.2.1",
        "198.18.0.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "fc00::1",
        "fe80::1",
        "ff00::1",
        "::ffff:127.0.0.1",
        "2001:db8::1",
        "2002:7f00:1::1",
        "3fff::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "142.250.180.14",
        "2607:f8b0:4007:80e::200e",
        "::ffff:142.250.180.14",
    ] {
        assert!(public_ip(ip.parse().unwrap()), "{ip}");
    }
}

async fn serve(response: String, delay: Duration) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).await;
        tokio::time::sleep(delay).await;
        let _ = socket.write_all(response.as_bytes()).await;
    });
    Url::parse(&format!("http://{address}/oembed")).unwrap()
}

#[tokio::test]
async fn downloaded_thumbnail_is_private_temporary_and_signature_checked() {
    use std::os::unix::fs::PermissionsExt;
    let client = Client::builder().no_proxy().build().unwrap();
    let payload = "RIFFxxxxWEBPpayload";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/webp\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let file = download_artwork(&client, serve(response, Duration::ZERO).await)
        .await
        .unwrap();
    let path = file.path().to_owned();
    assert_eq!(std::fs::read(&path).unwrap(), payload.as_bytes());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(file);
    assert!(!path.exists());
    let response =
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\n\r\noops".into();
    assert!(
        download_artwork(&client, serve(response, Duration::ZERO).await)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn bounded_transport_rejects_redirects_errors_types_sizes_and_timeouts() {
    // Loopback-only fixture: production client has HTTPS and public DNS guards.
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(200))
        .build()
        .unwrap();
    for (response, delay, succeeds) in [
        ("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".into(), Duration::ZERO, true),
        ("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/private\r\nContent-Length: 0\r\n\r\n".into(), Duration::ZERO, false),
        ("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".into(), Duration::ZERO, false),
        ("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\n\r\n{}".into(), Duration::ZERO, false),
        (format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", JSON_LIMIT + 1), Duration::ZERO, false),
        (format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}", "x".repeat(JSON_LIMIT + 1)), Duration::ZERO, false),
        ("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".into(), Duration::from_secs(1), false),
    ] {
        let url = serve(response, delay).await;
        assert_eq!(bounded_get(&client, url, JSON_LIMIT, &["application/json"]).await.is_ok(), succeeds);
    }
}
