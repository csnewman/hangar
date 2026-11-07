#!/usr/bin/env bash
# Checks the gpui-pre crates the app builds on against Zed's own source.
#
# gpui-pre-* are snapshots of Zed's gpui crates (and gpui-pre-reqwest of
# Zed's reqwest fork), republished to crates.io by gpui-component's
# maintainer, which gpui-component pins. Each says in its description which
# commit it is a snapshot of. This fetches that commit and compares every
# file the crate ships with Zed's, ignoring only Cargo's own files. A file
# that differs must say why, with a "Modified for gpui-pre" line; anything
# else fails the check. Run it whenever the gpui-component pin moves.
#
#   dev/verify-gpui-pre.sh            # the versions in crates/Cargo.lock
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cd "$repo/crates"
cargo fetch -q
registry=$(dirname "$(cargo metadata --format-version 1 | python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == "gpui-pre":
        print(p["manifest_path"])
')")
registry=$(dirname "$registry")

# The crates in the lock file, as name version.
crates=$(python3 - <<'EOF'
import tomllib
with open("Cargo.lock", "rb") as f:
    lock = tomllib.load(f)
for p in lock["package"]:
    if p["name"].startswith("gpui-pre"):
        print(p["name"], p["version"])
EOF
)

# checkout REPO SHA DIR: the commit's crates, without the rest of the history.
checkout() {
    git init -q "$3"
    git -C "$3" remote add origin "$1"
    git -C "$3" fetch -q --depth 1 --filter=blob:none origin "$2"
    git -C "$3" checkout -q FETCH_HEAD
}

# Where in Zed's tree each crate comes from, where its name does not say.
upstream_dir() {
    case "$1" in
    gpui-pre) echo crates/gpui ;;
    gpui-pre-derive-refineable) echo crates/refineable/derive_refineable ;;
    gpui-pre-reqwest) echo . ;;
    *)
        local base=${1#gpui-pre-}
        base=${base//-/_}
        if [ -d "$zed/crates/gpui_$base" ]; then echo "crates/gpui_$base"; else echo "crates/$base"; fi
        ;;
    esac
}

failed=0
zed="" zed_sha=""
while read -r name version; do
    src="$registry/$name-$version"
    desc=$(python3 -c "import tomllib,sys; print(tomllib.load(open(sys.argv[1],'rb'))['package'].get('description',''))" "$src/Cargo.toml")
    if [[ $desc =~ zed@([0-9a-f]{7,40}) ]]; then
        sha=${BASH_REMATCH[1]}
        if [ "$sha" != "$zed_sha" ]; then
            full=$(gh api "repos/zed-industries/zed/commits/$sha" --jq .sha)
            zed="$work/zed-$sha"
            checkout https://github.com/zed-industries/zed.git "$full" "$zed"
            zed_sha=$sha
        fi
        root=$zed
    elif [[ $desc =~ zed-industries/re[a-z]*@([0-9a-f]{7,40}) ]]; then
        sha=${BASH_REMATCH[1]}
        full=$(gh api "repos/zed-industries/reqwest/commits/$sha" --jq .sha)
        root="$work/reqwest-$sha"
        [ -d "$root" ] || checkout https://github.com/zed-industries/reqwest.git "$full" "$root"
    else
        echo "$name $version: no snapshot commit in its description: $desc"
        failed=1
        continue
    fi
    dir="$root/$(upstream_dir "$name")"
    if [ ! -d "$dir" ]; then
        echo "$name $version: no $dir upstream"
        failed=1
        continue
    fi
    unexplained=()
    modified=0
    while IFS= read -r f; do
        rel=${f#"$src"/}
        case "$rel" in Cargo.toml | Cargo.toml.orig | Cargo.lock | .cargo-ok | .cargo_vcs_info.json | LICENSE*) continue ;; esac
        # A build script that reads another of Zed's crates ships a copy of
        # what it reads under vendor/<crate>/.
        theirs="$dir/$rel"
        if [[ $rel =~ ^vendor/([^/]+)/(.*)$ ]]; then
            theirs="$root/crates/${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
        fi
        if [ -f "$theirs" ] && cmp -s "$f" "$theirs"; then
            continue
        fi
        if grep -q "Modified for gpui-pre" "$f"; then
            modified=$((modified + 1))
        else
            unexplained+=("$rel")
        fi
    done < <(find "$src" -type f)
    if [ ${#unexplained[@]} -gt 0 ]; then
        echo "$name $version: differs from upstream without saying why: ${unexplained[*]}"
        failed=1
    else
        echo "$name $version: matches upstream ($modified files marked as modified)"
    fi
done <<<"$crates"
exit $failed
