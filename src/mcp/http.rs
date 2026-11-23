//! Streamable HTTP for `kassi mcp --http`. Every tool is a plain request and
//! response, so requests are served statelessly as JSON. Every request must
//! carry the bearer token from `KASSI_MCP_TOKEN`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tracing::warn;

use super::Server;

const ENDPOINT: &str = "/mcp";
pub const TOKEN_VARIABLE: &str = "KASSI_MCP_TOKEN";
const MIN_TOKEN_LENGTH: usize = 32;

pub(super) async fn run(server: Server, bind: SocketAddr) -> Result<()> {
    let token = load_token(std::env::var(TOKEN_VARIABLE).ok().as_deref())?;
    if !bind.ip().is_loopback() {
        warn!(
            %bind,
            "serving without TLS; keep this address on a trusted network or VPN, or put a TLS proxy in front"
        );
    }
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("could not bind {bind}"))?;
    let address = listener.local_addr()?;
    eprintln!("listening url=http://{address}{ENDPOINT}");
    axum::serve(listener, router(server, token))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut terminate =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let terminate = async {
        match terminate.as_mut() {
            Some(signal) => {
                signal.recv().await;
            }
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = ctrl_c => {}
        () = terminate => {}
    }
}

fn router(server: Server, token: Arc<str>) -> Router {
    // Every request needs the token and clients reach the server under
    // deployment-specific names, so host checks are off. Browsers always send
    // Origin on these requests, so rejecting every Origin keeps them out.
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .disable_allowed_hosts()
        .enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route_service(ENDPOINT, service)
        .route_layer(middleware::from_fn_with_state(token, require_token))
}

async fn require_token(State(token): State<Arc<str>>, request: Request, next: Next) -> Response {
    if authorized(request.headers(), &token) {
        return next.run(request).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
    )
        .into_response()
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let Some((scheme, presented)) = value.to_str().ok().and_then(|value| value.split_once(' '))
    else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer")
        && bool::from(presented.as_bytes().ct_eq(token.as_bytes()))
}

fn load_token(value: Option<&str>) -> Result<Arc<str>> {
    let token = value.unwrap_or_default();
    if token.len() < MIN_TOKEN_LENGTH || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        bail!(
            "set {TOKEN_VARIABLE} to at least {MIN_TOKEN_LENGTH} visible ASCII characters, for example from `openssl rand -hex 32`"
        );
    }
    Ok(token.into())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn only_a_single_matching_bearer_credential_is_authorized() {
        let valid = format!("Bearer {TOKEN}");
        assert!(authorized(&headers(&[&valid]), TOKEN));
        assert!(authorized(&headers(&[&format!("bearer {TOKEN}")]), TOKEN));
        let prefix = format!("Bearer {}", &TOKEN[..31]);
        let extended = format!("Bearer {TOKEN}x");
        let basic = format!("Basic {TOKEN}");
        for value in [
            prefix.as_str(),
            extended.as_str(),
            basic.as_str(),
            TOKEN,
            "",
        ] {
            assert!(!authorized(&headers(&[value]), TOKEN), "{value:?}");
        }
        assert!(!authorized(&headers(&[]), TOKEN));
        assert!(!authorized(&headers(&[&valid, &valid]), TOKEN));
    }

    #[test]
    fn startup_refuses_unset_short_or_unprintable_tokens() {
        assert_eq!(&*load_token(Some(TOKEN)).unwrap(), TOKEN);
        let spaced = format!("{TOKEN} x");
        for value in [None, Some(""), Some(&TOKEN[..31]), Some(spaced.as_str())] {
            assert!(load_token(value).is_err(), "{value:?}");
        }
    }
}
