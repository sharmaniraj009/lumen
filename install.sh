#!/usr/bin/env bash
# Installs Lumen for the current user and binds it to a global hotkey.
#   ./install.sh                     -> Super+Space (frees it from input-source switching)
#   ./install.sh "<Control>space"    -> any other GNOME accelerator
#   ./install.sh --uninstall
set -euo pipefail

DEST="$HOME/.local/share/lumen-app"
APPS="$HOME/.local/share/applications"
AUTOSTART="$HOME/.config/autostart"
KB_PATH="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/lumen/"
KB_SCHEMA="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$KB_PATH"
MEDIA="org.gnome.settings-daemon.plugins.media-keys"
BACKUP="$DEST/input-source-keys.bak"

remove_binding() {
  local list
  list=$(gsettings get $MEDIA custom-keybindings)
  list=$(python3 -c "import ast,sys; l=ast.literal_eval(sys.argv[1].replace('@as ','')); l=[x for x in l if x!='$KB_PATH']; print(str(l))" "$list")
  gsettings set $MEDIA custom-keybindings "$list"
}

if [[ "${1:-}" == "--uninstall" ]]; then
  "$DEST/lumen" --quit 2>/dev/null || true
  remove_binding
  if [[ -f "$BACKUP" ]]; then
    gsettings set org.gnome.desktop.wm.keybindings switch-input-source "$(sed -n 1p "$BACKUP")"
    gsettings set org.gnome.desktop.wm.keybindings switch-input-source-backward "$(sed -n 2p "$BACKUP")"
  fi
  rm -rf "$DEST" "$APPS/lumen.desktop" "$AUTOSTART/lumen.desktop"
  echo "Lumen removed."
  exit 0
fi

KEY="${1:-<Super>space}"
SRC="$(cd "$(dirname "$0")" && pwd)"
[[ -x "$SRC/target/release/lumen" ]] || { echo "Build first: cargo build --release"; exit 1; }

mkdir -p "$DEST" "$APPS" "$AUTOSTART"
rm -f "$DEST/lumen.py" "$DEST/providers.py" "$DEST/lumen-toggle" "$DEST/lumen"  # unlink first: the running daemon keeps its copy
cp "$SRC/target/release/lumen" "$SRC/target/release/lumen-toggle" "$DEST/"
chmod +x "$DEST/lumen" "$DEST/lumen-toggle"

cat > "$APPS/lumen.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=Lumen Search
Comment=Search apps, files, the web, and do quick calculations
Exec=$DEST/lumen
Icon=system-search
Terminal=false
Categories=Utility;
NoDisplay=true
StartupNotify=false
DESK

cat > "$AUTOSTART/lumen.desktop" <<DESK
[Desktop Entry]
Type=Application
Name=Lumen Search (background)
Exec=$DEST/lumen --background
Icon=system-search
NoDisplay=true
X-GNOME-Autostart-enabled=true
DESK

# Super+Space is Ubuntu's default "switch input source" shortcut; move it out of the way.
if [[ "$KEY" == "<Super>space" ]]; then
  WM=org.gnome.desktop.wm.keybindings
  if [[ ! -f "$BACKUP" ]]; then
    { gsettings get $WM switch-input-source; gsettings get $WM switch-input-source-backward; } > "$BACKUP"
  fi
  gsettings set $WM switch-input-source "['XF86Keyboard']"
  gsettings set $WM switch-input-source-backward "['<Shift>XF86Keyboard']"
fi

remove_binding
list=$(gsettings get $MEDIA custom-keybindings)
list=$(python3 -c "import ast,sys; l=ast.literal_eval(sys.argv[1].replace('@as ','')); l.append('$KB_PATH'); print(str(l))" "$list")
gsettings set $MEDIA custom-keybindings "$list"
gsettings set "$KB_SCHEMA" name "Lumen Search"
gsettings set "$KB_SCHEMA" command "$DEST/lumen-toggle"
gsettings set "$KB_SCHEMA" binding "$KEY"

# Restart the background daemon so the new version is running.
"$DEST/lumen" --quit >/dev/null 2>&1 || true
sleep 0.5
nohup "$DEST/lumen" --background >/dev/null 2>&1 &
echo "Lumen installed. Press $KEY to search."
