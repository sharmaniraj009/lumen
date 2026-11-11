# Lumen (Rust + GTK4)

Spotlight-style search for Ubuntu. Press **Super+Space**.

```bash
cargo build --release && ./install.sh      # or ./install.sh "<Control>space"
./install.sh --uninstall                    # removes it and restores your old shortcut
```

Two binaries:

- `lumen`: the background daemon (started at login) that owns the window and the search.
- `lumen-toggle`: what the hotkey runs. It links only GIO and sends one D-Bus call, so the
  window appears in ~35 ms. It starts `lumen` itself if the daemon isn't running.

## Searches

Apps · calculator (`20% of 150`, `sqrt(2)^10`, `5!`) · units (`5 km to miles`, `70 f`) ·
currency (`100 usd to inr`) · world clock (`time in tokyo`) · weather (`weather in delhi`) ·
definitions (`define serendipity`) · Wikipedia · web suggestions · file names and document
contents · Settings panels (`wifi`) · system commands (`lock`, `dark mode`) · websites ·
paths (`~/Downloads`) · shell (`> htop`) · Slack people (`ravi`, `@ravi`).

**Ask Claude:** every query has an *Ask Claude* row; queries ending in `?` or starting with
`ask`, `claude` or `ai` put it on top. Enter streams the answer into the preview pane, and
Ctrl+C copies it. It uses your installed Claude Code CLI and its login, so it needs no API key.

## Slack people search

Type a colleague's name (or `slack ravi`, `@ravi`) to find them in your Slack workspace.
Enter opens a DM with them in the Slack app; Ctrl+Enter copies their email. The preview shows
their photo, title, status, local time and email.

One-time setup (about two minutes):

1. Go to <https://api.slack.com/apps> → **Create New App** → *From scratch*, and pick your workspace.
2. **OAuth & Permissions** → **User Token Scopes** → add `users:read` and `users:read.email`.
3. **Install to Workspace** (some workspaces need an admin to approve), then copy the
   **User OAuth Token** (starts with `xoxp-`).
4. Add it to `~/.config/lumen/config.json`: `"slack_token": "xoxp-…"` (or
   `"slack_tokens": ["xoxp-…", "xoxp-…"]` for several workspaces), then restart Lumen:
   `~/.local/share/lumen-app/lumen-toggle --quit; ~/.local/share/lumen-app/lumen --background &`

Lumen downloads the member directory once (cached in `~/.cache/lumen/slack-users.json`,
readable only by you) and refreshes it every 6 hours, so searching is instant and offline.
The token only reads the directory; it can't post or read messages.

## Settings (`~/.config/lumen/config.json`)

`search_engine` (google/duckduckgo/bing/brave) · `online` (false = no network lookups) ·
`claude` (false hides Ask Claude) · `claude_model` (default `haiku`; e.g. `sonnet`, `opus`) · `slack_token` / `slack_tokens` ·
`index_hidden` · `extra_index_paths` · `max_indexed_files`.
