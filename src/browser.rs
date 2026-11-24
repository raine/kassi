//! Runs the login in a browser window and reads the nonce it ends with.
//!
//! The login finishes with a request to `/callback?nonce=…` that answers 204,
//! so the nonce never shows up in the address bar. The browser is started with
//! a throwaway profile and watched over the DevTools protocol for that request.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;

/// How long the browser gets to start and open its DevTools port.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Chromium-based browsers, as macOS applications and as commands.
const MAC_APPS: &[&str] = &[
    "Google Chrome.app/Contents/MacOS/Google Chrome",
    "Chromium.app/Contents/MacOS/Chromium",
    "Brave Browser.app/Contents/MacOS/Brave Browser",
    "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "Vivaldi.app/Contents/MacOS/Vivaldi",
];
const COMMANDS: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "brave-browser",
    "microsoft-edge",
    "vivaldi",
];

/// The browser to log in with: `$KASSI_BROWSER`, else the first Chromium-based
/// browser found. `None` when the machine has none.
pub fn find() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KASSI_BROWSER").filter(|p| !p.is_empty()) {
        return Some(path.into());
    }
    let mut app_dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        app_dirs.push(PathBuf::from(home).join("Applications"));
    }
    let apps = app_dirs
        .iter()
        .flat_map(|dir| MAC_APPS.iter().map(move |app| dir.join(app)));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let commands = std::env::split_paths(&path)
        .flat_map(|dir| COMMANDS.iter().map(move |name| dir.join(name)))
        .collect::<Vec<_>>();
    apps.chain(commands).find(|candidate| candidate.is_file())
}

/// Opens `login_url` in `browser` and waits for the login to request
/// `callback_url` with a `nonce`, which it returns.
pub async fn login_nonce(browser: &Path, login_url: &str, callback_url: &str) -> Result<String> {
    let profile = tempfile::Builder::new()
        .prefix("kassi-login-")
        .tempdir()
        .context("creating a browser profile directory")?;
    let mut child = Command::new(browser)
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .args([
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--window-size=520,820",
            "about:blank",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", browser.display()))?;

    let nonce = tokio::select! {
        nonce = watch(profile.path(), &mut child, login_url, callback_url) => nonce,
        _ = tokio::signal::ctrl_c() => Err(anyhow!("login cancelled")),
    };
    // The profile directory can only be removed once the browser has exited.
    let _ = child.kill().await;
    nonce
}

async fn watch(
    profile: &Path,
    child: &mut Child,
    login_url: &str,
    callback_url: &str,
) -> Result<String> {
    let endpoint = tokio::time::timeout(STARTUP_TIMEOUT, devtools_endpoint(profile, child))
        .await
        .context("the browser did not open its DevTools port")??;
    let (mut socket, _) = tokio_tungstenite::connect_async(&endpoint)
        .await
        .context("connecting to the browser")?;

    let mut next_id = 0u64;
    let mut command = |method: &str, params: Value, session: Option<&str>| {
        next_id += 1;
        let mut message = json!({ "id": next_id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        Message::text(message.to_string())
    };

    // Attaches to the open tab and to any tab the login opens later.
    let attach = json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true });
    socket
        .send(command("Target.setAutoAttach", attach, None))
        .await?;

    let mut navigated = false;
    while let Some(message) = socket.next().await {
        let Message::Text(text) = message.context("reading from the browser")? else {
            continue;
        };
        let event: Value = serde_json::from_str(&text).context("decoding a browser message")?;
        let params = &event["params"];
        match event["method"].as_str() {
            Some("Target.attachedToTarget") if params["targetInfo"]["type"] == "page" => {
                let session = params["sessionId"].as_str();
                socket
                    .send(command("Network.enable", json!({}), session))
                    .await?;
                if !navigated {
                    navigated = true;
                    let page = json!({ "url": login_url });
                    socket.send(command("Page.navigate", page, session)).await?;
                }
            }
            Some("Network.requestWillBeSent") => {
                let url = params["request"]["url"].as_str().unwrap_or_default();
                if let Some(nonce) = callback_nonce(url, callback_url) {
                    return Ok(nonce);
                }
            }
            _ => {}
        }
    }
    bail!("the browser was closed before the login finished")
}

/// Reads the DevTools WebSocket address the browser writes into its profile
/// directory once the port is open.
async fn devtools_endpoint(profile: &Path, child: &mut Child) -> Result<String> {
    let file = profile.join("DevToolsActivePort");
    loop {
        if let Ok(text) = tokio::fs::read_to_string(&file).await {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://127.0.0.1:{port}{path}"));
            }
        }
        if let Some(status) = child.try_wait()? {
            bail!("the browser exited at startup ({status})");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The `nonce` of a request to the login callback, if `url` is one. The
/// identity provider's own URLs also carry a `nonce` parameter, so the address
/// has to match as well.
fn callback_nonce(url: &str, callback_url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let callback = url::Url::parse(callback_url).ok()?;
    if url.origin() != callback.origin() || url.path() != callback.path() {
        return None;
    }
    url.query_pairs()
        .find(|(key, _)| key == "nonce")
        .map(|(_, nonce)| nonce.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALLBACK: &str = "https://kirjautuminen.s-ostoslista.fi/callback";

    #[test]
    fn reads_the_nonce_only_from_the_login_callback() {
        assert_eq!(
            callback_nonce(&format!("{CALLBACK}?nonce=YWJj%3D%3D"), CALLBACK).as_deref(),
            Some("YWJj==")
        );
        assert_eq!(
            callback_nonce(&format!("{CALLBACK}?code=x&state=y"), CALLBACK),
            None
        );
        let authorize = "https://auth.tunnistus.s-ryhma.fi/oxauth/restv1/authorize?nonce=d25\
                         &redirect_uri=https%3A%2F%2Fkirjautuminen.s-ostoslista.fi%2Fcallback";
        assert_eq!(callback_nonce(authorize, CALLBACK), None);
    }
}
