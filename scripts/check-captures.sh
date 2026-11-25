#!/bin/sh
# Runs public DJ Link captures through Boothmate's parser: real packets from CDJ-2000nexus
# players and a DJM-2000nexus mixer, recorded by Deep Symmetry (github.com/Deep-Symmetry/dysentery).
# The captures are downloaded into target/, not part of this repository. Extra captures (your own
# club recordings, logs/djlink-*.pcap) can be passed as arguments.
#
#   scripts/check-captures.sh [capture ...]
set -eu
cd "$(dirname "$0")/.."
dir=target/captures/dysentery
if [ ! -d "$dir" ]; then
  git clone --depth 1 --quiet https://github.com/Deep-Symmetry/dysentery "$dir"
fi
cargo build --quiet
# shellcheck disable=SC2046
./target/debug/boothmate --check $(find "$dir/doc/assets" -name '*.pcap' -o -name '*.pcapng' | sort) "$@" > target/captures/report.txt \
  || { cat target/captures/report.txt; echo "Some DJ Link packets were not understood, see above."; exit 1; }
grep -E 'DJ Link$|should be' target/captures/report.txt
echo "All captures OK. Full report: target/captures/report.txt"
