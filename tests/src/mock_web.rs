use axum::{Router, extract::Query, response::Html, routing::get};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub struct MockWebServer {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MockWebServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel();

        let app = Router::new()
            .route("/", get(mock_baidu_index))
            .route("/s", get(mock_baidu_search))
            .route("/favicon.ico", get(mock_favicon))
            .route("/health", get(mock_health));

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .ok();
        });

        Self {
            addr,
            shutdown: Some(tx),
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for MockWebServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

async fn mock_baidu_index() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>百度一下</title></head><body><div id="content"><h1>百度一下，你就知道</h1></div></body></html>"#,
    )
}

#[derive(serde::Deserialize)]
struct SearchParams {
    wd: Option<String>,
}

async fn mock_baidu_search(Query(params): Query<SearchParams>) -> Html<String> {
    let query = params.wd.unwrap_or_default();
    Html(format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>百度搜索 - {}</title></head><body><h1>搜索结果: {}</h1></body></html>"#,
        query, query
    ))
}

async fn mock_favicon() -> &'static [u8] {
    // 1x1 transparent PNG
    &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x01, 0xE5, 0x27, 0xDE, 0xFC, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
        0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

async fn mock_health() -> &'static str {
    "ok"
}
