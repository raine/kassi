mod api;
mod auth;
mod mcp;

use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::api::Api;
use crate::auth::{AUTH_URL, Session, TokenFile};
use crate::mcp::{GetListArgs, Tools};

/// MCP server for the S-ostoslista shopping list.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Directory holding the session tokens [default: $XDG_STATE_HOME/kassi or
    /// ~/.local/state/kassi]
    #[arg(long, global = true, env = "KASSI_STATE_DIR")]
    state_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Log in with an S-ryhmä account and save the session.
    Login,
    /// Serve MCP on stdio, or on Streamable HTTP with --http.
    Mcp {
        /// Serve Streamable HTTP at /mcp instead of stdio. Requires
        /// KASSI_MCP_TOKEN, which clients send as a bearer token.
        #[arg(long)]
        http: bool,
        /// Address for --http.
        #[arg(long, default_value = "127.0.0.1:8737", requires = "http")]
        bind: SocketAddr,
    },
    /// Print the shopping lists as JSON.
    Lists,
    /// Print a shopping list and its items as JSON.
    Show {
        /// List id or name; may be omitted when there is only one list.
        list: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // stdout carries the MCP stdio transport, so logs go to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("KASSI_LOG").unwrap_or_else(|_| EnvFilter::new("kassi=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let state_dir = match cli.state_dir {
        Some(dir) => dir,
        None => auth::default_state_dir()?,
    };
    let token_file = TokenFile::in_dir(&state_dir);
    let http = reqwest::Client::builder()
        .user_agent(concat!("kassi/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?;

    match cli.command {
        Command::Login => login(http, token_file).await,
        Command::Mcp {
            http: serve_http,
            bind,
        } => {
            let api = open_api(http, token_file)?;
            if serve_http {
                mcp::run_http(api, bind).await
            } else {
                mcp::run_stdio(api).await
            }
        }
        Command::Lists => {
            let tools = Tools::new(open_api(http, token_file)?);
            print_json(&tools.list_shopping_lists().await?)
        }
        Command::Show { list } => {
            let tools = Tools::new(open_api(http, token_file)?);
            let args = GetListArgs {
                list,
                include_collected: Some(true),
            };
            print_json(&tools.get_shopping_list(args).await?)
        }
    }
}

fn open_api(http: reqwest::Client, token_file: TokenFile) -> Result<Api> {
    let session = Arc::new(Session::open(http.clone(), token_file)?);
    Ok(Api::new(http, session))
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn login(http: reqwest::Client, token_file: TokenFile) -> Result<()> {
    eprintln!(
        "\
Log in with a desktop browser:

  1. Open a new tab, open DevTools (Network tab) and tick \"Preserve log\".
  2. In that tab go to {AUTH_URL}/login and log in with your S-ryhmä account.
  3. The login ends on a blank page. In the Network tab, find the request
     `callback?nonce=…`, right-click it and choose Copy > Copy URL.
  4. Paste the URL here and press Enter. The nonce is single-use and expires
     within minutes.
"
    );
    eprint!("Callback URL: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading the callback URL")?;
    let nonce = auth::parse_nonce(&line)?;
    let tokens = auth::verify_nonce(&http, &nonce).await?;
    token_file.save(&tokens)?;

    let api = open_api(http, token_file.clone())?;
    let name = api
        .user_name()
        .await
        .context("logged in, but the API rejected the session")?;
    eprintln!(
        "Logged in{}. Session saved to {}",
        name.map(|n| format!(" as {n}")).unwrap_or_default(),
        token_file.path().display()
    );
    Ok(())
}
