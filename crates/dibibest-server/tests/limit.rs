mod common;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use dibibest_server::limit::RateLimiter;
use std::net::IpAddr;
use std::time::{Duration, Instant};

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn a_bucket_allows_a_burst_then_refuses_and_says_when_to_retry() {
    let limiter = RateLimiter::new(3);
    let t0 = Instant::now();
    for _ in 0..3 {
        assert!(limiter.check_at(ip("1.1.1.1"), t0).is_ok());
    }
    let wait = limiter.check_at(ip("1.1.1.1"), t0).unwrap_err();
    assert!(
        wait > Duration::ZERO && wait <= Duration::from_secs(60),
        "{wait:?}"
    );
}

#[test]
fn a_bucket_refills_with_time() {
    let limiter = RateLimiter::new(60); // one a second
    let t0 = Instant::now();
    for _ in 0..60 {
        limiter.check_at(ip("1.1.1.1"), t0).unwrap();
    }
    assert!(limiter.check_at(ip("1.1.1.1"), t0).is_err());
    assert!(limiter
        .check_at(ip("1.1.1.1"), t0 + Duration::from_secs(2))
        .is_ok());
}

#[test]
fn clients_do_not_share_a_bucket() {
    let limiter = RateLimiter::new(1);
    let t0 = Instant::now();
    limiter.check_at(ip("1.1.1.1"), t0).unwrap();
    assert!(limiter.check_at(ip("1.1.1.1"), t0).is_err());
    assert!(limiter.check_at(ip("2.2.2.2"), t0).is_ok());
}

#[test]
fn memory_is_bounded_when_many_clients_come_and_go() {
    let limiter = RateLimiter::new(10);
    let t0 = Instant::now();
    for i in 0..50_000u32 {
        let addr = IpAddr::from(std::net::Ipv4Addr::from(i));
        let _ = limiter.check_at(addr, t0 + Duration::from_millis(u64::from(i)));
    }
    assert!(
        limiter.tracked() <= 20_000,
        "{} entries kept",
        limiter.tracked()
    );
}

fn from(addr: &str) -> Request<Body> {
    Request::builder()
        .uri("/robots.txt")
        .header("x-forwarded-for", addr)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn the_app_answers_429_with_retry_after_once_the_budget_is_spent() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 3;
    let app = app(&fx);
    for _ in 0..3 {
        assert_eq!(send(&app, from("9.9.9.9")).await.status(), StatusCode::OK);
    }
    let refused = send(&app, from("9.9.9.9")).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(header(&refused, "retry-after").parse::<u64>().unwrap() >= 1);
}

#[tokio::test]
async fn probes_are_never_rate_limited() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 1;
    let app = app(&fx);
    for _ in 0..20 {
        assert_eq!(get(&app, "/healthz").await.status(), StatusCode::OK);
        assert_eq!(get(&app, "/readyz").await.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn a_forwarded_header_is_ignored_unless_the_proxy_is_trusted() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 2;
    fx.config.trusted_proxies = 0;
    let app = app(&fx);
    // A client that changes its own X-Forwarded-For must not get a fresh budget each time.
    let mut refused = 0;
    for i in 0..6 {
        let status = send(&app, from(&format!("10.0.0.{i}"))).await.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            refused += 1;
        }
    }
    assert!(
        refused >= 4,
        "spoofing the header escaped the limit ({refused} refused)"
    );
}

#[tokio::test]
async fn behind_a_trusted_proxy_each_forwarded_client_has_its_own_budget() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 2;
    fx.config.trusted_proxies = 1;
    let app = app(&fx);
    for _ in 0..2 {
        assert_eq!(send(&app, from("1.1.1.1")).await.status(), StatusCode::OK);
    }
    assert_eq!(
        send(&app, from("1.1.1.1")).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(send(&app, from("2.2.2.2")).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_proxys_own_entry_is_the_one_that_counts() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 1;
    fx.config.trusted_proxies = 1;
    let app = app(&fx);
    // "6.6.6.6" is what the client claimed; "1.1.1.1" is what the trusted proxy appended.
    assert_eq!(
        send(&app, from("6.6.6.6, 1.1.1.1")).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&app, from("7.7.7.7, 1.1.1.1")).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "a client-supplied entry must not choose the bucket"
    );
}

#[test]
fn addresses_in_one_ipv6_network_share_a_bucket() {
    let limiter = RateLimiter::new(1);
    let t0 = Instant::now();
    limiter.check_at(ip("2001:db8:0:1::1"), t0).unwrap();
    // Another address of the same /64: a client that owns the network must not get a fresh budget per address.
    assert!(limiter
        .check_at(ip("2001:db8:0:1:ffff:ffff:ffff:ffff"), t0)
        .is_err());
    // A different /64 is a different client.
    assert!(limiter.check_at(ip("2001:db8:0:2::1"), t0).is_ok());
}

#[test]
fn an_ipv4_mapped_address_is_the_same_client_as_the_ipv4_address() {
    let limiter = RateLimiter::new(1);
    let t0 = Instant::now();
    limiter.check_at(ip("1.2.3.4"), t0).unwrap();
    assert!(limiter.check_at(ip("::ffff:1.2.3.4"), t0).is_err());
}

fn lines(values: &[&str]) -> Request<Body> {
    let mut b = Request::builder().uri("/robots.txt");
    for v in values {
        b = b.header("x-forwarded-for", *v);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn a_proxy_that_adds_its_own_header_line_is_still_the_one_that_counts() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 1;
    fx.config.trusted_proxies = 1;
    let app = app(&fx);
    // The client wrote the first line; the proxy added the second.
    assert_eq!(
        send(&app, lines(&["6.6.6.6", "1.1.1.1"])).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&app, lines(&["7.7.7.7", "1.1.1.1"])).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the first header line is the client's and must not choose the bucket"
    );
}

#[tokio::test]
async fn with_two_trusted_proxies_the_client_is_the_second_entry_from_the_end() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 1;
    fx.config.trusted_proxies = 2;
    let app = app(&fx);
    // junk written by the client, the client as the first proxy saw it, the first proxy as the second saw it
    assert_eq!(
        send(&app, from("6.6.6.6, 1.1.1.1, 9.9.9.9")).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&app, from("7.7.7.7, 1.1.1.1, 9.9.9.9")).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        send(&app, from("7.7.7.7, 2.2.2.2, 9.9.9.9")).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_header_with_fewer_entries_than_trusted_proxies_is_not_believed() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 1;
    fx.config.trusted_proxies = 2;
    let app = app(&fx);
    // One entry cannot have been written by two proxies, so the connection's own address counts: both requests
    // share one bucket whatever the header says.
    assert_eq!(send(&app, from("1.1.1.1")).await.status(), StatusCode::OK);
    assert_eq!(
        send(&app, from("2.2.2.2")).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn an_entry_that_is_not_an_address_is_not_believed() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 1;
    fx.config.trusted_proxies = 1;
    let app = app(&fx);
    assert_eq!(send(&app, from("not-an-ip")).await.status(), StatusCode::OK);
    assert_eq!(
        send(&app, from("also-not")).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

fn raw_forwarded(bytes: &[u8]) -> Request<Body> {
    Request::builder()
        .uri("/robots.txt")
        .header(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(bytes).unwrap(),
        )
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn a_header_that_cannot_be_read_is_charged_to_its_own_bucket_not_the_proxys() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 2;
    fx.config.trusted_proxies = 1;
    let app = app(&fx);
    // Unreadable bytes (not visible ASCII): all such requests share one bucket and use it up...
    for _ in 0..2 {
        assert_eq!(
            send(&app, raw_forwarded(b"\xff\xfe")).await.status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        send(&app, raw_forwarded(b"\xff\xfe")).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the malformed requests are limited together"
    );
    // ...without touching the bucket of a request that reaches the server directly (no header at all).
    assert_eq!(get(&app, "/robots.txt").await.status(), StatusCode::OK);
    assert_eq!(get(&app, "/robots.txt").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_entry_that_is_not_an_address_does_not_use_up_the_direct_bucket_either() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 2;
    fx.config.trusted_proxies = 1;
    let app = app(&fx);
    for _ in 0..3 {
        let _ = send(&app, from("not-an-ip")).await;
    }
    assert_eq!(get(&app, "/robots.txt").await.status(), StatusCode::OK);
    assert_eq!(get(&app, "/robots.txt").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_unreadable_line_before_the_proxys_own_line_does_not_buy_a_better_bucket() {
    let mut fx = fixture();
    fx.config.rate_per_minute = 1;
    fx.config.trusted_proxies = 1;
    let app = app(&fx);
    let with_junk_line = |client: &'static str| {
        Request::builder()
            .uri("/robots.txt")
            .header(
                "x-forwarded-for",
                axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
            )
            .header("x-forwarded-for", client)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        send(&app, with_junk_line("1.1.1.1")).await.status(),
        StatusCode::OK
    );
    // A second client behind the same proxy with the same junk is limited too: junk never gives a fresh bucket.
    assert_eq!(
        send(&app, with_junk_line("2.2.2.2")).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}
