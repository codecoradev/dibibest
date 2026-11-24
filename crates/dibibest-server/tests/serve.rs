use dibibest_server::config::Config;
use dibibest_server::{bind, ServerError};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

fn config(dir: &std::path::Path) -> Config {
    let web = dir.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<html>INDEX</html>").unwrap();
    Config {
        addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.join("data"),
        web_dir: web,
        ..Config::default()
    }
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn it_serves_over_a_real_socket() {
    let dir = tempfile::tempdir().unwrap();
    let server = bind(config(dir.path())).await.unwrap();
    let addr = server.local_addr();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stopped.await;
    }));
    let health = http_get(addr, "/healthz").await;
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    let home = http_get(addr, "/").await;
    assert!(home.contains("INDEX"), "{home}");
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_second_server_on_the_same_data_directory_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let first = bind(config(dir.path())).await.unwrap();
    let second = bind(config(dir.path())).await.err().expect("must refuse");
    assert!(matches!(second, ServerError::Lock(_)), "{second:?}");
    drop(first);
}

#[tokio::test]
async fn shutdown_lets_go_of_the_data_directory_and_the_data_is_still_there() {
    let dir = tempfile::tempdir().unwrap();
    let server = bind(config(dir.path())).await.unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(server.run(async move {
        let _ = stopped.await;
    }));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    // The lock is free and the database opens again: a restart works.
    let again = bind(config(dir.path())).await.unwrap();
    drop(again);
    assert!(dir.path().join("data/dibibest.db").exists());
}

#[tokio::test]
async fn a_port_already_in_use_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut c = config(dir.path());
    c.addr = held.local_addr().unwrap();
    let err = bind(c).await.err().expect("must fail");
    assert!(matches!(err, ServerError::Io(_)), "{err:?}");
}
