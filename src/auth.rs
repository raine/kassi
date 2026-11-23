//! S-ostoslista sessions: redeeming a login nonce, persisting the token set,
//! and rotating it through `/refresh`.
//!
//! The refresh token is single-use. Every refresh returns a new one that must
//! be saved before the old one is forgotten, and two processes refreshing with
//! the same token would leave one of them logged out. Refreshes therefore hold
//! an exclusive lock on a file beside the token file and reread the tokens
//! under that lock, so a token rotated by another process is adopted instead
//! of spent twice.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

pub const AUTH_URL: &str = "https://kirjautuminen.s-ostoslista.fi";
/// Access tokens are refreshed when they have less than this long to live.
const EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    pub refresh_token: String,
}

impl TokenSet {
    fn access_expires_at(&self) -> Option<u64> {
        jwt_expiry(&self.access_token)
    }

    fn access_is_fresh(&self, now: u64) -> bool {
        self.access_expires_at()
            .is_some_and(|exp| exp > now + EXPIRY_MARGIN.as_secs())
    }
}

/// Reads the `exp` claim of a JWT without verifying it.
pub fn jwt_expiry(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp")?.as_u64()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Directory for the token file: `$KASSI_STATE_DIR`, else
/// `$XDG_STATE_HOME/kassi`, else `~/.local/state/kassi`.
pub fn default_state_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("KASSI_STATE_DIR") {
        return Ok(dir.into());
    }
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("kassi"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/kassi"))
}

/// The on-disk token file and its lock file.
#[derive(Clone, Debug)]
pub struct TokenFile {
    path: PathBuf,
    lock_path: PathBuf,
}

impl TokenFile {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            path: dir.join("tokens.json"),
            lock_path: dir.join("tokens.lock"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<TokenSet> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!(
                    "not logged in: {} does not exist; run `kassi login`",
                    self.path.display()
                )
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.path.display())),
        };
        serde_json::from_str(&text).with_context(|| format!("parsing {}", self.path.display()))
    }

    /// Writes the tokens readable only by the owner, replacing the file
    /// atomically so a crash never leaves a truncated token file.
    pub fn save(&self, tokens: &TokenSet) -> Result<()> {
        let dir = self.path.parent().context("token path has no parent")?;
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let tmp = self
            .path
            .with_extension(format!("json.tmp-{}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(serde_json::to_string_pretty(tokens)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;
        Ok(())
    }

    fn lock(&self) -> Result<File> {
        if let Some(dir) = self.lock_path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock_path)
            .with_context(|| format!("opening {}", self.lock_path.display()))?;
        file.lock()
            .with_context(|| format!("locking {}", self.lock_path.display()))?;
        Ok(file)
    }
}

/// Exchanges the `nonce` from the login callback for a token set.
pub async fn verify_nonce(http: &reqwest::Client, nonce: &str) -> Result<TokenSet> {
    let response = http
        .post(format!("{AUTH_URL}/verify"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_string(nonce)?)
        .send()
        .await
        .context("calling /verify")?;
    token_response(response, "/verify").await
}

async fn refresh(http: &reqwest::Client, refresh_token: &str) -> Result<TokenSet> {
    let response = http
        .post(format!("{AUTH_URL}/refresh"))
        .header(reqwest::header::CONTENT_TYPE, "text/plain")
        .body(refresh_token.to_owned())
        .send()
        .await
        .context("calling /refresh")?;
    token_response(response, "/refresh").await
}

async fn token_response(response: reqwest::Response, endpoint: &str) -> Result<TokenSet> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{endpoint} failed with {status}: {}", truncate(&body, 300));
    }
    serde_json::from_str(&body).with_context(|| format!("{endpoint} returned an unexpected body"))
}

fn truncate(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// Extracts the login nonce from a pasted callback URL, or accepts a bare nonce.
pub fn parse_nonce(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        bail!("no callback URL given");
    }
    if !input.contains("://") {
        return Ok(input.to_owned());
    }
    let url = url::Url::parse(input).context("not a valid URL")?;
    if let Some((_, nonce)) = url.query_pairs().find(|(k, _)| k == "nonce") {
        return Ok(nonce.into_owned());
    }
    if url.query_pairs().any(|(k, _)| k == "code") {
        bail!(
            "this is the `callback?code=` URL; copy the next request, `callback?nonce=`, instead"
        );
    }
    Err(anyhow!("the URL has no `nonce` parameter"))
}

/// A logged-in session that hands out fresh access tokens.
pub struct Session {
    http: reqwest::Client,
    file: TokenFile,
    tokens: Mutex<TokenSet>,
}

impl Session {
    pub fn open(http: reqwest::Client, file: TokenFile) -> Result<Self> {
        let tokens = file.load()?;
        Ok(Self {
            http,
            file,
            tokens: Mutex::new(tokens),
        })
    }

    /// Returns an access token that is valid for at least a few more minutes.
    pub async fn access_token(&self) -> Result<String> {
        let mut tokens = self.tokens.lock().await;
        if tokens.access_is_fresh(now()) {
            return Ok(tokens.access_token.clone());
        }
        self.refresh_locked(&mut tokens, None).await?;
        Ok(tokens.access_token.clone())
    }

    /// Refreshes after the API rejected `rejected`, unless another caller has
    /// already replaced it.
    pub async fn access_token_after_rejection(&self, rejected: &str) -> Result<String> {
        let mut tokens = self.tokens.lock().await;
        if tokens.access_token != rejected {
            return Ok(tokens.access_token.clone());
        }
        self.refresh_locked(&mut tokens, Some(rejected)).await?;
        Ok(tokens.access_token.clone())
    }

    async fn refresh_locked(&self, tokens: &mut TokenSet, rejected: Option<&str>) -> Result<()> {
        let file = self.file.clone();
        let lock = tokio::task::spawn_blocking(move || file.lock()).await??;

        // Another process may have rotated the tokens since they were loaded.
        match self.file.load() {
            Ok(on_disk) if on_disk.refresh_token != tokens.refresh_token => {
                info!("adopting tokens rotated by another process");
                *tokens = on_disk;
                if rejected != Some(tokens.access_token.as_str()) && tokens.access_is_fresh(now()) {
                    drop(lock);
                    return Ok(());
                }
            }
            Ok(_) => {}
            Err(e) => warn!("could not reread token file: {e:#}"),
        }

        let fresh = refresh(&self.http, &tokens.refresh_token)
            .await
            .context("refreshing the session failed; run `kassi login` again")?;
        *tokens = fresh;
        if let Err(e) = self.file.save(tokens) {
            // The old refresh token is already spent; keep serving from memory.
            warn!("could not save rotated tokens: {e:#}");
        }
        info!(
            expires_at = tokens.access_expires_at(),
            "refreshed access token"
        );
        drop(lock);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(exp: u64) -> String {
        let claims = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        format!("eyJhbGciOiJIUzI1NiJ9.{claims}.sig")
    }

    #[test]
    fn reads_expiry_from_jwt_and_rejects_opaque_tokens() {
        assert_eq!(jwt_expiry(&jwt(1_800_000_000)), Some(1_800_000_000));
        assert_eq!(jwt_expiry("opaque-refresh-token"), None);
        assert_eq!(jwt_expiry("a.!!!.c"), None);
    }

    #[test]
    fn access_token_counts_as_stale_inside_the_margin() {
        let tokens = |exp| TokenSet {
            access_token: jwt(exp),
            id_token: None,
            refresh_token: "r".into(),
        };
        let now = 1_000_000;
        assert!(tokens(now + 3600).access_is_fresh(now));
        assert!(!tokens(now + 60).access_is_fresh(now));
        assert!(!tokens(now - 1).access_is_fresh(now));
    }

    #[test]
    fn parses_nonce_from_callback_url_or_bare_value() {
        assert_eq!(
            parse_nonce(" https://kirjautuminen.s-ostoslista.fi/callback?nonce=YWJj%3D%3D \n")
                .unwrap(),
            "YWJj=="
        );
        assert_eq!(parse_nonce("YWJj").unwrap(), "YWJj");
        let code = parse_nonce("https://kirjautuminen.s-ostoslista.fi/callback?code=x&state=y");
        assert!(code.unwrap_err().to_string().contains("callback?nonce="));
        assert!(parse_nonce("https://example.com/").is_err());
        assert!(parse_nonce("  ").is_err());
    }

    #[test]
    fn token_file_round_trips_with_owner_only_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let file = TokenFile::in_dir(&dir.path().join("state"));
        assert!(file.load().unwrap_err().to_string().contains("kassi login"));
        let tokens = TokenSet {
            access_token: "a".into(),
            id_token: Some("i".into()),
            refresh_token: "r".into(),
        };
        file.save(&tokens).unwrap();
        assert_eq!(file.load().unwrap(), tokens);
        let mode = fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn reads_the_token_file_the_auth_service_returns() {
        let body = r#"{"accessToken":"a","idToken":"i","refreshToken":"r"}"#;
        let tokens: TokenSet = serde_json::from_str(body).unwrap();
        assert_eq!(tokens.refresh_token, "r");
    }
}
