//! Real localhost HTTP protocol tests for provider cooldowns, without credentials.
use jarvis::providers::{CrusoeClient, NEMOTRON_MODEL, ProviderConfig, ProviderError};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn reject(status: u16, header: &str) -> ProviderError {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let header = header.to_owned();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = "PRIVATE_ERROR_BODY fixture-secret-key";
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\n{header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let client = CrusoeClient::new(
        "fixture-secret-key",
        NEMOTRON_MODEL,
        ProviderConfig::loopback_for_tests(&endpoint, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    let error = client
        .complete("fixture system", "fixture question", 10)
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(!error.to_string().contains("PRIVATE_ERROR_BODY"));
    assert!(!error.to_string().contains("fixture-secret-key"));
    error
}

#[tokio::test]
async fn numeric_retry_after_survives_transport_and_is_not_replaced_with_generic_backoff() {
    let error = reject(429, "Retry-After: 3\r\n").await;
    assert_eq!(
        error,
        ProviderError::RateLimitedAfter(Duration::from_secs(3))
    );
    assert!(error.retryable());
    assert_eq!(error.retry_delay(0), Some(Duration::from_secs(3)));
    assert_eq!(error.retry_delay(1), Some(Duration::from_secs(3)));
}
#[tokio::test]
async fn missing_or_malformed_cooldown_uses_bounded_rate_limit_backoff() {
    for header in [
        "",
        "Retry-After: nonsense\r\n",
        "Retry-After: -3\r\n",
        "Retry-After: 1.5\r\n",
    ] {
        let error = reject(429, header).await;
        assert_eq!(error, ProviderError::RateLimited);
        assert_eq!(error.retry_delay(0), Some(Duration::from_secs(2)));
        assert_eq!(error.retry_delay(1), Some(Duration::from_secs(4)));
        assert!(
            error.retry_delay(u32::MAX).unwrap() <= Duration::from_secs(60),
            "backoff grows without bound"
        );
    }
}
#[tokio::test]
async fn server_delay_beyond_operation_deadline_is_not_clamped_to_an_early_retry() {
    let error = reject(429, "Retry-After: 86400\r\n").await;
    assert!(error.retry_delay(0).unwrap() >= Duration::from_secs(60));
}
#[tokio::test]
async fn cooldown_headers_cannot_turn_authentication_into_a_retryable_error() {
    let error = reject(401, "Retry-After: 1\r\n").await;
    assert_eq!(error, ProviderError::Authentication);
    assert_eq!(error.retry_delay(0), None);
    assert!(!error.retryable());
    assert_eq!(
        reject(503, "").await.retry_delay(0),
        Some(Duration::from_millis(350))
    );
}

#[tokio::test]
async fn http_date_cooldown_is_relative_to_now_and_large_numeric_delays_do_not_overflow() {
    let future = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(10));
    let error = reject(429, &format!("Retry-After: {future}\r\n")).await;
    let delay = error.retry_delay(0).unwrap();
    assert!(
        delay >= Duration::from_secs(8) && delay <= Duration::from_secs(10),
        "HTTP-date deadline was not honored"
    );
    let excessive = reject(429, "Retry-After: 184467440737095516160\r\n").await;
    assert_eq!(excessive.retry_delay(0), Some(Duration::MAX));
}
#[tokio::test]
async fn zero_and_past_date_cooldowns_keep_a_small_nonzero_retry_floor() {
    for header in [
        "Retry-After: 0\r\n",
        "Retry-After: Thu, 01 Jan 1970 00:00:00 GMT\r\n",
    ] {
        let error = reject(429, header).await;
        assert_eq!(error.retry_delay(0), Some(Duration::from_millis(250)));
    }
}

#[tokio::test]
async fn duplicate_retry_headers_use_longest_valid_server_cooldown() {
    let error = reject(
        429,
        "Retry-After: 1\r\nRetry-After: invalid\r\nRetry-After: 7\r\nRetry-After: 2\r\n",
    )
    .await;
    assert_eq!(error.retry_delay(0), Some(Duration::from_secs(7)));
}
