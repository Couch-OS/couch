#!/usr/bin/env bash
# Can the last released Couch still start on a configuration this tree saved,
# and does this tree read that release's files without changing a byte?
#
# A remote that is rolled back runs the old daemon against the file the new one
# wrote. If the old model cannot parse or validate it, couch-confd exits and the
# remote has no configuration at all. The model's own tests mirror the old
# reader with frozen types; this runs the real thing: the model source at the
# release tag, built beside this tree's, exchanging files in both directions.
#
#   tools/tests/config-crossload.sh            # against the pinned release
#   COUCH_CROSSLOAD_TAG=<tag> tools/tests/...  # against another one
#
# Needs git, cargo and the release tag (fetched if this clone lacks it). Builds
# in a temporary directory and removes it; nothing is written to the checkout.
# Both copies build with --locked against their own model/Cargo.lock: the
# program is an example inside each couch-model copy, so it adds no dependency.
set -euo pipefail

TAG="${COUCH_CROSSLOAD_TAG:-v0.1.0-alpha.20260919.188}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/couch-crossload.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if ! git -C "$ROOT" rev-parse --quiet --verify "refs/tags/$TAG^{commit}" >/dev/null; then
  echo "fetching $TAG"
  git -C "$ROOT" fetch --quiet --depth 1 origin "refs/tags/$TAG:refs/tags/$TAG"
fi

mkdir -p "$WORK/old" "$WORK/new" "$WORK/files"
git -C "$ROOT" archive "$TAG" model | tar -x -C "$WORK/old"
# The working tree, not HEAD: this has to be able to fail before a commit.
(cd "$ROOT" && tar -c --exclude target model) | tar -x -C "$WORK/new"

for side in old new; do
  mkdir -p "$WORK/$side/model/couch-model/examples"
  cp "$ROOT/tools/tests/config-crossload.rs" "$WORK/$side/model/couch-model/examples/crossload.rs"
  echo "building the $side model$([ "$side" = old ] && echo " ($TAG)")"
  CARGO_TARGET_DIR="$WORK/target-$side" cargo build --quiet --locked \
    --manifest-path "$WORK/$side/model/Cargo.toml" -p couch-model --example crossload
done
old="$WORK/target-old/debug/examples/crossload"
new="$WORK/target-new/debug/examples/crossload"
cd "$WORK/files"

failures=0
rows=()
# cell <command...>: one cell of the matrix; the output of a failure is kept.
# After a failed write there is no file, so the rest of that row is skipped.
cell() {
  local out width="$1"
  shift
  if [ -n "$skip" ]; then
    row+="$(printf "%-${width}s" -)"
  elif out="$("$@" 2>&1)"; then
    row+="$(printf "%-${width}s" ok)"
  else
    row+="$(printf "%-${width}s" FAIL)"
    failures=$((failures + 1))
    [ "$2" = write ] && skip=1
    problems+="  $name: ${*/#$WORK\/target-/}"$'\n'"$(printf '%s\n' "$out" | cut -c1-400 | sed 's/^/      /' | head -5)"$'\n'
  fi
}
same() { cmp -s "$1" "$2" || { echo "$1 and $2 differ"; diff "$1" "$2" | head -5; return 1; }; }
expect_layers() { [ "$("$new" layers "$1")" = "$2" ] || { echo "$1 has layers $("$new" layers "$1"), expected $2"; return 1; }; }
old_view_matches() {
  "$old" load "new-$1.json" >"old-view-$1.json" || return 1
  "$new" load-without-v3 "new-$1.json" >"new-v2-$1.json" || return 1
  same "old-view-$1.json" "new-v2-$1.json"
}
old_save_wins() {
  "$old" resave "new-$1.json" "old-resave-$1.json" || return 1
  "$new" load "old-resave-$1.json" >"after-$1.json" || return 1
  "$new" load-without-v3-migrated "new-$1.json" >"want-$1.json" || return 1
  same "after-$1.json" "want-$1.json" || return 1
  "$new" leaks "old-resave-$1.json" && expect_layers "old-resave-$1.json" "$2"
}
new_reads_its_own() {
  "$new" load "new-$1.json" >"new-view-$1.json" || return 1
  "$new" show "$1" >"built-$1.json" || return 1
  same "new-view-$1.json" "built-$1.json"
}
same_config() {
  "$old" load "old-$1.json" >"old-own-$1.json" || return 1
  "$new" load "old-$1.json" >"new-own-$1.json" || return 1
  same "old-own-$1.json" "new-own-$1.json"
}
same_bytes() {
  "$new" rewrite "old-$1.json" "rewritten-$1.json" || return 1
  same "old-$1.json" "rewritten-$1.json"
}

problems="" name="" skip=""
echo
echo "this tree writes, $TAG reads"
printf '  %-2s %-44s %-9s %s\n' "" "" layers "writes layers no-leak old-loads old=v2 old-save-wins reads-own"
while IFS='|' read -r name layers old_layers label; do
  row="" skip=""
  cell 7 "$new" write "$name" "new-$name.json"
  cell 7 expect_layers "new-$name.json" "$layers"
  cell 8 "$new" leaks "new-$name.json"
  cell 10 "$old" load "new-$name.json"
  cell 7 old_view_matches "$name"
  cell 14 old_save_wins "$name" "$old_layers"
  cell 9 new_reads_its_own "$name"
  printf '  %-2s %-44s %-9s %s\n' "$name" "$label" "$layers" "$row"
done <<'STATES'
A|plain|plain|no package
B|v1|v1|protocol 1 package
C|v1+v2|v1+v2|protocol 2: dB control, spaced input
D|v1+v2+v3|v1+v2|protocol 3: x: ids in all six places
E|v1+v2+v3|v1+v2|D on a connection the Denon pilot converted
F|v1+v3|v1|x: ids on a protocol 1 package (no v2 layer)
STATES

echo
echo "$TAG writes, this tree reads"
printf '  %-2s %-44s %s\n' "" "" "writes same-config same-bytes"
while IFS='|' read -r name label; do
  row="" skip=""
  cell 7 "$old" write "$name" "old-$name.json"
  cell 12 same_config "$name"
  cell 10 same_bytes "$name"
  printf '  %-2s %-44s %s\n' "$name" "$label" "$row"
done <<'STATES'
A|no package
B|protocol 1 package
C|protocol 2: dB control, spaced input
Cd|C on a connection the Denon pilot converted
STATES

echo
echo "G: x: ids this tree must refuse to save"
"$new" refuses || { failures=$((failures + 1)); problems+="    an x: id was accepted where no package declares it"$'\n'; }

echo
if [ "$failures" -gt 0 ]; then
  echo "config cross-load: $failures check(s) failed"
  printf '%s' "$problems"
  exit 1
fi
echo "config cross-load: every check passed"
