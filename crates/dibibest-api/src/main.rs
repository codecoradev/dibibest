use dibibest_api::{app, config::Oidc, store::Store};

#[tokio::main]
async fn main() {
    let oidc = Oidc::from_env();
    if oidc.is_none() {
        eprintln!("OIDC not configured: auth and saved projects are disabled");
    }
    let db = std::env::var("DIBIBEST_API_DB").unwrap_or_else(|_| "./dibibest.db".into());
    let store = Store::open(&db).unwrap_or_else(|e| panic!("cannot open {db}: {e}"));
    let addr = std::env::var("DIBIBEST_API_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".into());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("cannot bind {addr}: {e}"));
    eprintln!("dibibest-api listening on {addr}");
    axum::serve(listener, app::router(app::AppState::new(oidc, store)))
        .await
        .expect("server");
}
