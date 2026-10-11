#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")" && pwd)"
cd "$root"

icon="$root/assets/sound-route.png"
desktop="$root/sound-route.desktop"

if [[ ! -f "$icon" ]]; then
  echo "Missing $icon" >&2
  exit 1
fi

cargo build --release

sudo install -d \
  /usr/share/applications \
  /usr/share/pixmaps \
  /usr/share/icons/hicolor/512x512/apps \
  /usr/share/icons/hicolor/256x256/apps

sudo install -m 755 "$root/target/release/sound-route" /usr/bin/sound-route
sudo install -m 644 "$icon" /usr/share/pixmaps/sound-route.png
sudo install -m 644 "$icon" /usr/share/icons/hicolor/512x512/apps/sound-route.png
sudo install -m 644 "$desktop" /usr/share/applications/sound-route.desktop

icon256="$(mktemp --suffix=.png)"
if command -v magick >/dev/null; then
  magick "$icon" -resize 256x256 PNG32:"$icon256"
else
  cp "$icon" "$icon256"
fi
sudo install -m 644 "$icon256" /usr/share/icons/hicolor/256x256/apps/sound-route.png
rm -f "$icon256"

if command -v gtk-update-icon-cache >/dev/null; then
  sudo gtk-update-icon-cache -f /usr/share/icons/hicolor >/dev/null
fi
if command -v update-desktop-database >/dev/null; then
  sudo update-desktop-database /usr/share/applications >/dev/null
fi

echo "Installed /usr/bin/sound-route"
echo "Installed /usr/share/applications/sound-route.desktop"
echo "Installed /usr/share/pixmaps/sound-route.png"
