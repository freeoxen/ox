use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use clap::Parser;
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

#[derive(Debug, Deserialize)]
struct Config {
    port: u16,
    host: String,
    api_key: String,
    static_dir: Option<String>,
    wasm_pkg_dir: Option<String>,
    js_pkg_dir: Option<String>,
}

#[derive(Debug, Parser, Serialize)]
#[command(name = "ox-dev-server")]
struct CliArgs {
    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,

    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,

    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,

    #[arg(long, default_value = "ox-dev-server.toml")]
    #[serde(skip)]
    config: String,

    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    static_dir: Option<String>,

    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    wasm_pkg_dir: Option<String>,

    #[arg(long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    js_pkg_dir: Option<String>,
}

fn load_config() -> Result<Config, Box<figment::Error>> {
    let cli = CliArgs::parse();

    Figment::new()
        .merge(Serialized::defaults(serde_json::json!({
            "port": 0,
            "host": "127.0.0.1"
        })))
        .merge(Toml::file(&cli.config))
        .merge(
            Env::raw()
                .only(&["ANTHROPIC_API_KEY"])
                .map(|_| "api_key".into()),
        )
        .merge(Env::prefixed("OX_"))
        .merge(Serialized::defaults(&cli))
        .extract()
        .map_err(Box::new)
}

struct AppState {
    api_key: String,
    client: reqwest::Client,
    /// Directory where wasm-pack output lives.
    wasm_pkg_dir: Option<String>,
    /// Directory where the bundled JS/CSS output lives.
    js_pkg_dir: Option<String>,
    /// Directory where static HTML files live.
    static_dir: Option<String>,
}

#[tokio::main]
async fn main() {
    let config = load_config().unwrap_or_else(|e| {
        eprintln!("configuration error: {e}");
        std::process::exit(1);
    });

    let static_dir = config
        .static_dir
        .or_else(|| find_dir(&["crates/ox-web/static", "static"]));
    let wasm_pkg_dir = config
        .wasm_pkg_dir
        .or_else(|| find_dir(&["target/wasm-pkg", "pkg"]));
    let js_pkg_dir = config.js_pkg_dir.or_else(|| find_dir(&["target/js-pkg"]));

    if static_dir.is_none() {
        eprintln!("warning: could not find static dir (crates/ox-web/static)");
    }
    if wasm_pkg_dir.is_none() {
        eprintln!(
            "warning: could not find wasm pkg dir (target/wasm-pkg). Run wasm-pack build first."
        );
    }
    if js_pkg_dir.is_none() {
        eprintln!("warning: could not find js pkg dir (target/js-pkg). Run bun build first.");
    }

    let state = Arc::new(AppState {
        api_key: config.api_key,
        client: reqwest::Client::new(),
        wasm_pkg_dir,
        js_pkg_dir,
        static_dir,
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/pkg/{*path}", get(serve_wasm_pkg))
        .route("/dist/{*path}", get(serve_js_pkg))
        .route("/complete", post(proxy_complete))
        .route("/health", get(health))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", config.host, config.port))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    println!("ox-dev-server listening on http://{addr}");
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

async fn serve_js_pkg(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Some(ref dir) = state.js_pkg_dir {
        let file_path = format!("{dir}/{path}");
        if let Ok(contents) = tokio::fs::read(&file_path).await {
            let content_type = match file_path.rsplit('.').next() {
                Some("js") => "application/javascript",
                Some("css") => "text/css",
                Some("map") => "application/json",
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
    }
    None
}
