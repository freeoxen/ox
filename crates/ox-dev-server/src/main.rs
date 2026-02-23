use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use futures::StreamExt;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

struct AppState {
    api_key: String,
    client: reqwest::Client,
    /// Directory where wasm-pack output lives.
    wasm_pkg_dir: Option<String>,
    /// Directory where static HTML files live.
    static_dir: Option<String>,
}

#[tokio::main]
async fn main() {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    // Resolve the static and wasm directories relative to the workspace root.
    // When run via `cargo run -p ox-dev-server` the CWD is the workspace root.
    let static_dir = find_dir(&["crates/ox-web/static", "static"]);
    let wasm_pkg_dir = find_dir(&["target/wasm-pkg", "pkg"]);

    if static_dir.is_none() {
        eprintln!("warning: could not find static dir (crates/ox-web/static)");
    }
    if wasm_pkg_dir.is_none() {
        eprintln!(
            "warning: could not find wasm pkg dir (target/wasm-pkg). Run wasm-pack build first."
        );
    }

    let state = Arc::new(AppState {
        api_key,
        client: reqwest::Client::new(),
        wasm_pkg_dir,
        static_dir,
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/pkg/{*path}", get(serve_wasm_pkg))
        .route("/complete", post(proxy_complete))
        .route("/health", get(health))
        .layer(cors)
        .with_state(state);

    let addr = "0.0.0.0:3000";
    println!("ox-dev-server listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> &'static str {
    "ok"
}

async fn serve_index(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if let Some(ref dir) = state.static_dir {
        let path = format!("{dir}/index.html");
        if let Ok(contents) = tokio::fs::read_to_string(&path).await {
            return Html(contents).into_response();
        }
    }
    (StatusCode::NOT_FOUND, "index.html not found").into_response()
}

async fn serve_wasm_pkg(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Some(ref dir) = state.wasm_pkg_dir {
        let file_path = format!("{dir}/{path}");
        if let Ok(contents) = tokio::fs::read(&file_path).await {
            let content_type = match file_path.rsplit('.').next() {
                Some("js") => "application/javascript",
                Some("wasm") => "application/wasm",
                Some("json") => "application/json",
                _ => "application/octet-stream",
            };
            return (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, content_type)],
                contents,
            )
                .into_response();
        }
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

async fn proxy_complete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let _ = headers; // unused for now

    let response = match state
        .client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &state.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response();
        }
    };

    let status = response.status();
    let upstream_content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    // Stream the response body back to the client
    let byte_stream = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let body = Body::from_stream(byte_stream);

    Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY))
        .header("content-type", upstream_content_type)
        .header("cache-control", "no-cache")
        .body(body)
        .unwrap()
        .into_response()
}

fn find_dir(candidates: &[&str]) -> Option<String> {
    for candidate in candidates {
        let path = std::path::Path::new(candidate);
        if path.is_dir() {
            return Some(candidate.to_string());
        }
        // Also try relative to CARGO_MANIFEST_DIR at compile time
    }
    None
}
