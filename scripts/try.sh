#!/bin/sh
# Download the CI build of a branch or pull request and run it. macOS, needs the gh CLI.
#
#   scripts/try.sh            latest build of main
#   scripts/try.sh 2          build of pull request #2
#   scripts/try.sh fix/foo    build of a branch
#
# Builds go to ~/Boothmate-try/<branch>/Boothmate. A boothmate.toml in ~/Boothmate-try is
# copied into each build, so your settings stay the same. A running Boothmate is stopped first.
set -eu

repo=yastefan/boothmate
ref=${1:-main}
root=${BOOTHMATE_TRY:-$HOME/Boothmate-try}

case $ref in
  *[!0-9]*) branch=$ref ;;
  *) branch=$(gh pr view "$ref" -R "$repo" --json headRefName -q .headRefName) ;;
esac

run=$(gh run list -R "$repo" -w build -b "$branch" -L 1 --json databaseId -q '.[0].databaseId')
[ -n "$run" ] || { echo "No build for $branch yet."; exit 1; }
status=$(gh run view "$run" -R "$repo" --json status -q .status)
if [ "$status" != completed ]; then
  echo "Build $run for $branch is still running, waiting ..."
  gh run watch "$run" -R "$repo" --exit-status >/dev/null
fi
[ "$(gh run view "$run" -R "$repo" --json conclusion -q .conclusion)" = success ] \
  || { echo "Build $run for $branch failed: gh run view $run -R $repo"; exit 1; }

dir="$root/$branch"
old_lua=""
[ -f "$dir/Boothmate/lua/boothmate.lua" ] && old_lua=$(cat "$dir/Boothmate/lua/boothmate.lua")
rm -rf "$dir"
mkdir -p "$dir"
gh run download "$run" -R "$repo" -n Boothmate-mac -D "$dir"
ditto -x -k "$dir/Boothmate-mac.zip" "$dir"
rm "$dir/Boothmate-mac.zip"
xattr -dr com.apple.quarantine "$dir/Boothmate" 2>/dev/null || true
[ -f "$root/boothmate.toml" ] && cp "$root/boothmate.toml" "$dir/Boothmate/"

if [ -n "$old_lua" ] && [ "$old_lua" != "$(cat "$dir/Boothmate/lua/boothmate.lua")" ]; then
  echo "Note: the Lua plugin changed. Import $dir/Boothmate/lua/boothmate.xml into grandMA3 again."
fi

pkill -x boothmate-mac 2>/dev/null && sleep 1 || true
echo "Starting $branch (build $run). Ctrl+C to stop."
(sleep 2 && open "http://127.0.0.1:8088/") &
exec "$dir/Boothmate/boothmate-mac"
