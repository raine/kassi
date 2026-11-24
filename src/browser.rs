//! Runs the login in a browser window and reads the nonce it ends with.
//!
//! The login finishes with a request to `/callback?nonce=…` that answers 204,
//! so the nonce never shows up in the address bar. The browser is started with
//! a throwaway profile and watched over the DevTools protocol for that request.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// How long the browser gets to start and open its DevTools port.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the browser gets to quit after being told to.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

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
    let mut child = launch(browser, profile.path())?;

    let endpoint = tokio::time::timeout(
        STARTUP_TIMEOUT,
        devtools_endpoint(profile.path(), &mut child),
    )
    .await
    .context("the browser did not open its DevTools port")??;
    let (socket, _) = tokio_tungstenite::connect_async(&endpoint)
        .await
        .context("connecting to the browser")?;
    let mut browser = Browser { socket, next_id: 0 };

    let nonce = tokio::select! {
        nonce = browser.watch(login_url, callback_url) => nonce,
        _ = tokio::signal::ctrl_c() => Err(anyhow!("login cancelled")),
    };

    let _ = tokio::time::timeout(EXIT_TIMEOUT, browser.close()).await;
    let _ = child.kill().await;
    remove_profile(profile).await;
    nonce
}

/// Removes the profile directory, which the browser's processes can still be
/// writing to for a moment after the browser has quit.
async fn remove_profile(profile: tempfile::TempDir) {
    let path = profile.keep();
    for _ in 0..20 {
        if tokio::fs::remove_dir_all(&path).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Starts the browser on an empty tab with `profile` as its profile directory.
/// The returned process is the browser itself, or on macOS the `open` command
/// that started it, which exits right away.
fn launch(browser: &Path, profile: &Path) -> Result<Child> {
    // A macOS application started through its executable opens behind the
    // other windows; `open` brings it to the front. `-n` starts an instance
    // separate from one already running.
    let mut command = match app_bundle(browser) {
        Some(app) => {
            let mut command = Command::new("open");
            command.args(["-n", "-a"]).arg(app).arg("--args");
            command
        }
        None => Command::new(browser),
    };
    command
        .arg(format!("--user-data-dir={}", profile.display()))
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
        .with_context(|| format!("starting {}", browser.display()))
}

/// The `.app` bundle an executable at `<bundle>/Contents/MacOS/<name>` is in.
fn app_bundle(executable: &Path) -> Option<&Path> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.ends_with("Contents/MacOS") && bundle.extension()? == "app").then_some(bundle)
}

/// The browser's DevTools connection.
struct Browser {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Browser {
    async fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<()> {
        self.next_id += 1;
        let mut message = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        self.socket.send(Message::text(message.to_string())).await?;
        Ok(())
    }

    /// Quits the browser and waits for it to go away.
    async fn close(&mut self) {
        let _ = self.send("Browser.close", json!({}), None).await;
        while let Some(Ok(_)) = self.socket.next().await {}
    }

    /// Opens `login_url` in the browser's tab and waits for a request to
    /// `callback_url` that carries a nonce.
    async fn watch(&mut self, login_url: &str, callback_url: &str) -> Result<String> {
        // Attaches to the open tab and to any tab the login opens later.
        let attach =
            json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true });
        self.send("Target.setAutoAttach", attach, None).await?;

        let mut tabs = HashSet::new();
        let mut navigated = false;
        while let Some(message) = self.socket.next().await {
            let Message::Text(text) = message.context("reading from the browser")? else {
                continue;
            };
            let event: Value = serde_json::from_str(&text).context("decoding a browser message")?;
            let params = &event["params"];
            let session = params["sessionId"].as_str();
            match event["method"].as_str() {
                Some("Target.attachedToTarget") if params["targetInfo"]["type"] == "page" => {
                    tabs.extend(session.map(str::to_owned));
                    self.send("Network.enable", json!({}), session).await?;
                    if !navigated {
                        navigated = true;
                        let page = json!({ "url": login_url });
                        self.send("Page.navigate", page, session).await?;
                    }
                }
                // On macOS the browser keeps running after its last window
                // closes, so the connection staying open says nothing.
                Some("Target.detachedFromTarget") => {
                    if session.is_some_and(|session| tabs.remove(session)) && tabs.is_empty() {
                        break;
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
        // The launcher exiting cleanly leaves the browser running.
        if let Some(status) = child.try_wait()?.filter(|status| !status.success()) {
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

    #[test]
    fn finds_the_app_bundle_of_a_macos_executable() {
        let chrome = Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        assert_eq!(
            app_bundle(chrome),
            Some(Path::new("/Applications/Google Chrome.app"))
        );
        assert_eq!(app_bundle(Path::new("/usr/bin/chromium")), None);
    }
}
