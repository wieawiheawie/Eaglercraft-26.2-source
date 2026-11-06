#!/usr/bin/env bash
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo_root="$(cd "$root/.." && pwd)"
source_resources="$repo_root/game/src/main/resources"
destination="${1:-$root/dist}"

if [[ ! -d "$source_resources/assets/minecraft" ]]; then
  echo "Minecraft assets not found at $source_resources/assets/minecraft" >&2
  exit 2
fi

mkdir -p "$destination"
cp -a "$source_resources/assets" "$destination/"
cp -a "$source_resources/data" "$destination/"
for file in pack.mcmeta pack.png version.json; do
  if [[ -f "$source_resources/$file" ]]; then
    cp "$source_resources/$file" "$destination/$file"
  fi
done

echo "Copied $(find "$destination/assets" -type f | wc -l) asset files and $(find "$destination/data" -type f | wc -l) data files."
