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
paths (`~/Downloads`) · shell (`> htop`).

**Ask Claude:** every query has an *Ask Claude* row; queries ending in `?` or starting with
`ask`, `claude` or `ai` put it on top. Enter streams the answer into the preview pane, and
Ctrl+C copies it. It uses your installed Claude Code CLI and its login, so it needs no API key.

## Settings (`~/.config/lumen/config.json`)

`search_engine` (google/duckduckgo/bing/brave) · `online` (false = no network lookups) ·
`claude` (false hides Ask Claude) · `claude_model` (default `haiku`; e.g. `sonnet`, `opus`) ·
`index_hidden` · `extra_index_paths` · `max_indexed_files`.
