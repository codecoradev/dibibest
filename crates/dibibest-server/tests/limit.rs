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
    fx.config.trust_proxy = false;
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
    fx.config.trust_proxy = true;
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
    fx.config.trust_proxy = true;
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
