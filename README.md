# kassi

An [MCP](https://modelcontextprotocol.io) server for
[S-ostoslista](https://play.google.com/store/apps/details?id=fi.sok.s_ostoslista),
the S-ryhmä shopping list app. It lets an assistant such as Claude or ChatGPT
read and edit your shopping lists, search products in your store, and look up
stores. It is a single static binary that runs comfortably on a Raspberry Pi.

kassi uses the app's own backend with your S-ryhmä account. It is not
affiliated with S-ryhmä, and the API it relies on is undocumented and may
change.

## Tools

| Area     | Tools                                                                                                                                                                       |
| -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Lists    | `list_shopping_lists`, `get_shopping_list`, `create_shopping_list`, `update_shopping_list`, `duplicate_shopping_list`, `delete_shopping_list`, `share_shopping_list`, `join_shared_list` |
| Items    | `add_items`, `add_products`, `update_items`, `set_item_product`, `remove_items`, `clear_collected`                                                                           |
| Products | `search_products`, `get_product`, `get_recommendations`, `purchase_history`                                                                                                 |
| Stores   | `search_stores`, `get_store`                                                                                                                                                |

Lists hold two kinds of items: free text ("maito") and products linked to an
item in the store's assortment, which carry a price and their place in the
store. Product search with a `store_id` returns that store's prices, campaigns,
and locations: an aisle number or a department such as MAITO-OSASTO, and the
shelf name.

`purchase_history` lists what you have bought with your S-Etukortti, by
category, as products at a chosen store with current prices. The API only
offers this as suggestions for a list, so kassi briefly creates an empty list
with that store and deletes it again. Product search in a store uses the same
history: products you have bought come first and are marked `bought_before`.
kassi keeps a store's history for an hour, so only the first search there pays
for the temporary list.

## Install

Build and install locally:

```sh
just install            # cargo install --locked --path .
```

Or build a static binary for a 64-bit Raspberry Pi OS and copy it over SSH
(needs [zig](https://ziglang.org) and `cargo-zigbuild` 0.24 or newer):

```sh
just deploy-pi pi@raspberrypi.local   # installs ~/.local/bin/kassi on the Pi
```

## Log in

```sh
kassi login
```

The S-ryhmä login needs a browser, and a captcha keeps it from being
automated, so `kassi login` walks you through it:

1. In a desktop browser, open DevTools on the Network tab and enable
   "Preserve log".
2. Open `https://kirjautuminen.s-ostoslista.fi/login` in that tab and log in.
3. The login ends on a blank page. In the Network tab, find the
   `callback?nonce=…` request and copy its URL.
4. Paste the URL into `kassi login` within a few minutes.

You can run `kassi login` on the Pi over SSH and do the browser part on any
computer.

The session is saved to `$XDG_STATE_HOME/kassi/tokens.json` (by default
`~/.local/state/kassi/tokens.json`, readable only by you); set
`KASSI_STATE_DIR` or `--state-dir` to keep it elsewhere. kassi refreshes the
session on its own. The refresh token is single-use and each refresh replaces
it, so do not copy `tokens.json` to a second machine and use both: the first
refresh logs the other one out. Logging out in the app also ends the session.
Run `kassi login` again when that happens.

Check that it works:

```sh
kassi lists
kassi show "Ruoka"
```

## Connect an assistant

### Claude Code, Claude Desktop, and other local clients (stdio)

Run kassi as a subprocess, either locally or on the Pi over SSH:

```sh
claude mcp add kassi -- kassi mcp
claude mcp add kassi -- ssh pi@raspberrypi.local .local/bin/kassi mcp
```

For Claude Desktop, add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "kassi": { "command": "ssh", "args": ["pi@raspberrypi.local", ".local/bin/kassi", "mcp"] }
  }
}
```

### ChatGPT through an OpenAI tunnel

ChatGPT cannot start a local process, but OpenAI's
[`tunnel-client`](https://github.com/openai/tunnel-client) can run `kassi mcp`
on your machine and connect it to ChatGPT over an outbound connection, with no
public port or HTTPS setup.

1. On the [OpenAI Platform](https://platform.openai.com/settings/organization/tunnels),
   create a tunnel for your ChatGPT workspace and copy its `tunnel_…` ID.
2. Create an API key restricted to **Tunnels: Read and Use**, and save it in a
   file readable only by you.
3. Point `tunnel-client` at kassi:

   ```yaml
   config_version: 1
   control_plane:
     base_url: "https://api.openai.com"
     tunnel_id: "tunnel_…"
     api_key: "file:/home/you/.config/kassi-mcp/openai-tunnel-key"
   mcp:
     commands:
       - channel: main
         command: "/home/you/.local/bin/kassi mcp"
   ```

   and run `tunnel-client run --config <that file>`, for example as a systemd
   service.
4. In ChatGPT, open **Plugins → Add → Add custom MCP server**, choose
   **Tunnel**, pick the tunnel, and set authentication to **No
   authentication**.

ChatGPT keeps the tool list it loaded when you added the server. After
updating kassi to a version whose tools, tool descriptions, or instructions
changed, open **Settings → Plugins**, choose the server, press **Refresh
tools**, and start a new chat; until then ChatGPT works from the old
descriptions.

kassi logs each tool call with its arguments to stderr, which tunnel-client
passes on (to the journal when it runs under systemd). That shows which tools
ChatGPT actually called.

ChatGPT asks for approval before tools that delete things, such as
`remove_items` and `delete_shopping_list`. Anyone who can use the plugin in
your ChatGPT workspace can read and change your lists.

### Over the network (Streamable HTTP)

```sh
export KASSI_MCP_TOKEN=$(openssl rand -hex 32)
kassi mcp --http --bind 127.0.0.1:8737
```

The endpoint is `http://<host>:8737/mcp`. Every request needs the token,
either as `Authorization: Bearer <token>` or in the URL as
`/mcp/<token>` for clients that cannot send headers. kassi serves plain HTTP,
so keep it on localhost or a private network (for example Tailscale) and put
HTTPS in front of it before exposing it.

To reach it from outside your network, put HTTPS in front of it, for example
with [Tailscale Funnel](https://tailscale.com/kb/1223/funnel) (`tailscale funnel
--bg 8737`), and use `https://<host>/mcp/<token>` as the server URL. The token in
the URL is the only thing protecting your account, so treat the URL like a
password.

### Running as a service on the Pi

`~/.config/systemd/user/kassi.service`:

```ini
[Unit]
Description=kassi MCP server
After=network-online.target

[Service]
ExecStart=%h/.local/bin/kassi mcp --http --bind 127.0.0.1:8737
EnvironmentFile=%h/.config/kassi/env
Restart=on-failure

[Install]
WantedBy=default.target
```

Put `KASSI_MCP_TOKEN=...` in `~/.config/kassi/env` (mode 600), then:

```sh
systemctl --user daemon-reload
systemctl --user enable --now kassi
loginctl enable-linger "$USER"   # keep it running while logged out
```

Logs go to stderr (`journalctl --user -u kassi`); set `KASSI_LOG=kassi=debug`
for more detail.

## Development

```sh
just            # list recipes
just check      # format, unused dependencies, clippy
just test       # unit tests
just build-pi   # static aarch64 binary
```

Run `just install-hooks` once to enable the commit hooks.
