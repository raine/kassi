# kassi

An [MCP](https://modelcontextprotocol.io) server that lets Claude, ChatGPT, and
other assistants use your
[S-ostoslista](https://play.google.com/store/apps/details?id=fi.sok.s_ostoslista)
shopping lists.

kassi is an unofficial, independent project. It is not affiliated with or
endorsed by S-ryhmä or the S-ostoslista app. It uses your S-ryhmä account and
depends on an undocumented API that may change. Using it is probably against
some ToS, so use it at your own risk.

https://github.com/user-attachments/assets/fa19e33d-4c75-4ab8-9b46-98e2ea7a447f

## What the assistant can do

- View, create, rename, copy, and delete lists, set a list's store, share a list
  by link, and join a list someone shared.
- Add, rename, check off, and remove items, set quantities, and clear the
  checked-off ones.
- Search a store's products with prices, campaigns, and shelf locations, and
  look up a product's ingredients, allergens, and nutrition.
- See what you have bought with your S-Etukortti and get suggestions for a list.
  Products you have bought before come first in searches.
- Find stores and their address, opening hours, and services.
- Order a list's products into a walk from the entrance to the checkout, drawn
  on the store's floor plan.
- Turn a list into a link to the S-kaupat online store, where one click adds
  the list's products to your cart. You pick the store and time and pay there
  yourself; kassi does not place orders.

Try asking:

- "Add milk and rye bread to my Ruoka list."
- "What do I usually buy that's missing from my list?"
- "When does Prisma Kalasatama close today?"
- "Show me the route through the store for my list."
- "Give me a link to order my list from S-kaupat."

Good to know:

- An item is either a note ("maito"), as in the app, or a product from the
  store, which has a price and a place in the store. Each list has its own
  store, which the assistant uses for prices and products. Only products appear
  on the route map.
- ChatGPT shows the route map in the chat. Other clients get the walking order
  as text.
- Reading your purchase history briefly creates an empty list and deletes it
  again, so you may see one flash by in the app.

## Install

With Rust 1.96 or newer, run in this repository:

```sh
cargo install --locked --path .
```

To run kassi on a Raspberry Pi instead, see [Raspberry Pi](#raspberry-pi).

## Log in

```sh
kassi login
```

kassi opens a browser window for the login and closes it when you are done. The
window is a fresh profile of Chrome or another Chromium-based browser, so your
saved passwords are not available in it. Set `KASSI_BROWSER` to the browser's
executable to pick which one is used.

### Without a browser

On a machine with no Chromium-based browser, or with `kassi login --paste`, you
log in with a browser on any computer and give the result to kassi:

1. In a desktop browser, open a new tab, open DevTools on the Network tab, and
   enable **Preserve log**.
2. Go to `https://kirjautuminen.s-ostoslista.fi/login` in that tab and log in.
3. The login ends on a blank page. In the Network tab, find the
   `callback?nonce=…` request and copy its URL.
4. Paste the URL into `kassi login` within a few minutes.

### After logging in

Check that it works:

```sh
kassi lists          # all your lists
kassi show "Ruoka"   # one list and its items
```

kassi keeps the session alive on its own. Run `kassi login` again if you log out
in the app or the session stops working.

## Connect your assistant

### Claude Code, Claude Desktop, and other local clients

The client runs `kassi mcp` itself:

```sh
claude mcp add kassi -- kassi mcp
```

For Claude Desktop, add this to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "kassi": { "command": "/absolute/path/to/kassi", "args": ["mcp"] }
  }
}
```

### ChatGPT

ChatGPT cannot start a local program. OpenAI's
[`tunnel-client`](https://github.com/openai/tunnel-client) runs kassi on your
machine and connects it to ChatGPT, with no public port or HTTPS setup.

1. On the
   [OpenAI Platform](https://platform.openai.com/settings/organization/tunnels),
   create a tunnel for your ChatGPT workspace and copy its `tunnel_…` ID.

2. Create an API key restricted to **Tunnels: Read and Use**, and save it in a
   file readable only by you.

3. Save a config file for `tunnel-client`:

   ```yaml
   config_version: 1
   control_plane:
     base_url: "https://api.openai.com"
     tunnel_id: "tunnel_…"
     api_key: "file:/absolute/path/to/openai-tunnel-key"
   mcp:
     commands:
       - channel: main
         command: "/absolute/path/to/kassi mcp"
   ```

4. Run `tunnel-client run --config <that file>` and keep it running, for example
   as a systemd service.

5. In ChatGPT, open **Plugins → Add → Add custom MCP server**, choose
   **Tunnel**, pick the tunnel, and set authentication to **No authentication**.

Anyone who can use the plugin in your ChatGPT workspace can read and change your
lists.

After updating kassi, open **Settings → Plugins** in ChatGPT, choose the server,
press **Refresh tools**, and start a new chat. Until then ChatGPT uses the old
tool descriptions.

### Over HTTP

For clients that connect to a URL:

```sh
export KASSI_MCP_TOKEN=$(openssl rand -hex 32)
kassi mcp --http                # listens on 127.0.0.1:8737; change with --bind
```

The endpoint is `http://<host>:8737/mcp`. Every request needs the token, either
as an `Authorization: Bearer <token>` header or in the URL as `/mcp/<token>` for
clients that cannot send headers.

kassi serves plain HTTP, so keep it on localhost or a private network such as
Tailscale. To reach it from the internet, put HTTPS in front of it, for example
with [Tailscale Funnel](https://tailscale.com/kb/1223/funnel)
(`tailscale funnel --bg 8737`), and use `https://<host>/mcp/<token>` as the
server URL. That URL is then the only thing protecting your account, so treat it
like a password.

## Raspberry Pi

kassi runs well on a Pi with a 64-bit OS. Build it on your computer and copy it
over SSH. This needs [just](https://github.com/casey/just),
[zig](https://ziglang.org), and `cargo-zigbuild` 0.24 or newer.

```sh
just deploy-pi pi@raspberrypi.local   # installs ~/.local/bin/kassi on the Pi
```

The Pi has no browser to log in with, so log in on your computer and move the
session to the Pi:

```sh
dir=$(mktemp -d)
kassi --state-dir "$dir" login
ssh pi@raspberrypi.local 'mkdir -p -m 700 ~/.local/state/kassi'
scp "$dir/tokens.json" pi@raspberrypi.local:.local/state/kassi/
rm -r "$dir"
```

The temporary directory keeps this login apart from your computer's own. A
session only works in one place: copying the computer's own `tokens.json` would
log out whichever machine refreshes it second.

You can also log in on the Pi itself over SSH and do the
[browser steps](#without-a-browser) on your computer:

```sh
ssh -t pi@raspberrypi.local .local/bin/kassi login
```

Local clients can run kassi on the Pi through SSH:

```sh
claude mcp add kassi -- ssh pi@raspberrypi.local .local/bin/kassi mcp
```

To serve it over HTTP, create `~/.config/systemd/user/kassi.service` on the Pi:

```ini
[Unit]
Description=kassi MCP server
After=network-online.target

[Service]
ExecStart=%h/.local/bin/kassi mcp --http
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

## Session file

The login is saved in `~/.local/state/kassi/tokens.json` (under
`$XDG_STATE_HOME` if set). Set `KASSI_STATE_DIR` or pass `--state-dir` to keep
it elsewhere. Do not copy this file to a second machine and use both: they log
each other out.

## Tools

| Tool                      | What it does                                                             |
| ------------------------- | ------------------------------------------------------------------------ |
| `list_shopping_lists`     | Lists your shopping lists with their store and open item count           |
| `get_shopping_list`       | Shows a list's items, with quantity, price, and place in the store       |
| `create_shopping_list`    | Creates a list, optionally with a store and initial notes                |
| `update_shopping_list`    | Renames a list or changes its store                                      |
| `duplicate_shopping_list` | Copies a list under a new name                                           |
| `delete_shopping_list`    | Deletes a list and its items                                             |
| `share_shopping_list`     | Creates a link that lets someone else open and edit a list               |
| `join_shared_list`        | Joins a list from its share link                                         |
| `order_link`              | Makes an s-kaupat.fi link for adding a list's products to the cart       |
| `add_items`               | Adds notes to a list                                                     |
| `add_products`            | Adds products to a list, with quantities                                 |
| `update_items`            | Checks items off or back on, renames them, or sets quantities            |
| `set_item_product`        | Links a note to a product, or unlinks it                                 |
| `remove_items`            | Deletes items from a list                                                |
| `clear_collected`         | Deletes every checked-off item from a list                               |
| `search_products`         | Searches products by name, with a store's prices and shelf locations     |
| `get_product`             | Shows a product's price, ingredients, allergens, nutrition, and origin   |
| `get_recommendations`     | Suggests products for a list from your purchase history                  |
| `purchase_history`        | Lists the products you have bought with your S-Etukortti, by category    |
| `search_stores`           | Finds stores by name, district, or city                                  |
| `get_store`               | Shows a store's address, phone, opening hours, and services              |
| `show_route_map`          | Shows a store's floor plan, with the walking route for a list's products |

## Troubleshooting

kassi logs every tool call and its arguments to stderr, which shows what the
assistant actually did. The log can contain your list contents. Under systemd,
read it with `journalctl --user -u kassi`. Set `KASSI_LOG=kassi=debug` for more
detail.

## Development

```sh
just                         # list recipes
just install-quality-tools   # tools the checks need
just install-hooks           # enable the commit hooks
just check                   # formatting, unused dependencies, clippy
just test                    # unit tests
```
