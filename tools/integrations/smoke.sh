#!/bin/sh
# Build, sign, index, and admit every catalogued integration in an isolated
# ARMv7 Alpine test repository. This never uses a release key or a device.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd -P)
catalog="$root/integrations/catalog.json"
output=${1:-"$root/build/integration-smoke"}
case "$output" in /*) ;; *) output="$PWD/$output";; esac
[ ! -e "$output" ] || { echo "output must not already exist: $output" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq is required to read $catalog" >&2; exit 1; }
jq -e '.schema == 1 and (.integrations | type == "array" and length > 0)' "$catalog" >/dev/null || { echo "invalid integration catalog" >&2; exit 1; }
[ -x "$root/daemon/target/armv7-unknown-linux-musleabihf/release/couch-confd" ] || { echo "build ARMv7 couch-confd first" >&2; exit 1; }

entries=$(mktemp "${TMPDIR:-/tmp}/couch-integration-catalog.XXXXXX")
trap 'rm -f "$entries"' EXIT HUP INT TERM
jq -r '.integrations[] | [.id, .manifest, .binary] | @tsv' "$catalog" > "$entries"
while IFS="$(printf '\t')" read -r id manifest binary; do
    [ -n "$id" ] && [ -f "$root/$manifest" ] && [ -x "$root/clients/target/armv7-unknown-linux-musleabihf/release/$binary" ] || {
        echo "catalog entry $id is missing its ARMv7 binary or manifest" >&2; exit 1
    }
done < "$entries"

docker run --rm --platform linux/arm/v7 alpine:3.21 uname -m | grep -Eq 'arm|aarch' || {
    echo "Docker cannot execute linux/arm/v7 containers (QEMU/binfmt is required)" >&2; exit 1
}
mkdir -p "$output"

docker run --rm --platform linux/arm/v7 \
    -v "$root:/src:ro" -v "$output:/out" alpine:3.21 sh -ec '
        apk add --no-cache alpine-sdk binutils jq busybox-extras >/dev/null
        catalog=/src/integrations/catalog.json
        abuild-keygen -a -n
        key=$(find /root/.abuild -name "*.rsa" -print -quit)
        cp "$key.pub" /out/; cp "$key.pub" /etc/apk/keys/
        jq -r ".integrations[] | [.id, .manifest, .binary] | @tsv" "$catalog" >/tmp/catalog.tsv
        : >/out/packages.tsv
        while IFS="$(printf "\t")" read -r id manifest binary; do
            version=$(jq -er .version "/src/$manifest")
            package=$(/src/tools/integrations/build-apk.sh "$id" "$version" \
                "/src/clients/target/armv7-unknown-linux-musleabihf/release/$binary" \
                "/src/$manifest" "$key" /out/packages)
            printf "%s\t%s\t%s\n" "$id" "$version" "$package" >>/out/packages.tsv
        done </tmp/catalog.tsv
        /src/tools/integrations/build-repository.sh "$key" /out/packages /out/test-repository
        while IFS="$(printf "\t")" read -r id version package; do apk --keys-dir /root/.abuild verify "$package"; done </out/packages.tsv
        busybox-extras httpd -p 18080 -h /out/test-repository
        printf "%s\n" http://127.0.0.1:18080 >/tmp/repositories
        apk --arch armv7 --keys-dir /root/.abuild --repositories-file /tmp/repositories --no-cache update
        while IFS="$(printf "\t")" read -r id version package; do apk --arch armv7 --keys-dir /root/.abuild --repositories-file /tmp/repositories --no-cache search -x "couch-integration-$id"; done </out/packages.tsv
        confd=/src/daemon/target/armv7-unknown-linux-musleabihf/release/couch-confd
        {
            while IFS="$(printf "\t")" read -r id version package; do
                echo "signed sideload $id"
                "$confd" integrations --root "/tmp/sideload-$id" --keys-dir /root/.abuild install-sideload "$package"
                "$confd" integrations --root "/tmp/sideload-$id" list | grep -Fx "$id $version"
                echo "signed repository $id"
                "$confd" integrations --root /tmp/repository-store --keys-dir /root/.abuild install-repository "couch-integration-$id" --repository http://127.0.0.1:18080
            done </out/packages.tsv
            "$confd" integrations --root /tmp/repository-store list >/tmp/repository-list
            while IFS="$(printf "\t")" read -r id version package; do grep -Fx "$id $version" /tmp/repository-list; done </out/packages.tsv
            IFS="$(printf "\t")" read -r first_id first_version first_package </out/packages.tsv
            echo "untrusted signature rejection $first_id"
            mkdir /tmp/untrusted-keys
            if "$confd" integrations --root /tmp/untrusted-store --keys-dir /tmp/untrusted-keys install-sideload "$first_package"; then echo "unexpected successful untrusted install" >&2; exit 1; fi
            test ! -e "/tmp/untrusted-store/state/$first_id"
            echo "tampered archive rejection $first_id"
            cp "$first_package" /tmp/tampered.apk; printf x >>/tmp/tampered.apk
            if "$confd" integrations --root /tmp/tampered-store --keys-dir /root/.abuild install-sideload /tmp/tampered.apk; then echo "unexpected successful tampered install" >&2; exit 1; fi
            test ! -e "/tmp/tampered-store/state/$first_id"
        } >/out/lifecycle.log 2>&1
    '
# Receipts were emitted inside the container as /out/...; make them usable by
# the host. The repository is deliberately a test repository; no output is a
# production feed, even though preview and test-only entries are both tested.
awk -F '\t' -v OFS='\t' -v output="$output" '{ if (substr($3, 1, 5) == "/out/") $3 = output substr($3, 5); print }' \
    "$output/packages.tsv" > "$output/packages.tsv.host"
mv "$output/packages.tsv.host" "$output/packages.tsv"
echo "native catalog admission receipt: $output"
