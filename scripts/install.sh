#!/bin/sh
# Install the release matching this checkout. Source builds are explicit opt-in.
set -eu

root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
cd "$root"
version=$(awk '/^version[[:space:]]*=/ {gsub(/"/, "", $3); print $3; exit}' herdr-plugin.toml)
case "$version" in
    ''|*[!0-9.]*) echo "Cannot read plugin release version." >&2; exit 1 ;;
esac
mkdir -p bin
stage=$(mktemp -d "$root/bin/.install.XXXXXXXX")
trap 'rm -rf "$stage"' EXIT
trap 'exit 1' HUP INT TERM

if [ "${HERDR_OMNI_BUILD_FROM_SOURCE:-0}" = 1 ]; then
    cargo build --release --locked --bin herdr-omni --target-dir "$root/target"
    cp target/release/herdr-omni "$stage/herdr-omni"
else
    case "$(uname -s)" in
        Darwin) platform=apple-darwin ;;
        Linux) platform=unknown-linux-musl ;;
        *) echo "Herdr Omni supports macOS and Linux only." >&2; exit 1 ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64) arch=x86_64 ;;
        arm64|aarch64) arch=aarch64 ;;
        *) echo "No Herdr Omni binary is available for this architecture." >&2; exit 1 ;;
    esac
    asset="herdr-omni-$arch-$platform"
    url="https://github.com/mmjang/herdr-omni/releases/download/v$version/$asset"
    download() {
        if command -v curl >/dev/null 2>&1; then
            curl --fail --location --silent --show-error --retry 2 \
                --connect-timeout 15 --max-time 180 \
                --proto '=https' --proto-redir '=https' -o "$2" "$1"
        elif command -v wget >/dev/null 2>&1; then
            wget --https-only --timeout=30 --tries=3 -q -O "$2" "$1"
        else
            echo "Install curl or wget to download Herdr Omni." >&2
            return 1
        fi
    }
    if ! download "$url" "$stage/$asset" || ! download "$url.sha256" "$stage/$asset.sha256"; then
        echo "Unable to download Herdr Omni v$version for $arch-$platform. Check that this release includes binary assets. No source build was attempted." >&2
        exit 1
    fi
    expected=$(awk 'NR == 1 {print $1}' "$stage/$asset.sha256")
    case "$expected" in
        ''|*[!0-9a-fA-F]*) echo "Invalid release checksum." >&2; exit 1 ;;
    esac
    [ "${#expected}" -eq 64 ] || { echo "Invalid release checksum." >&2; exit 1; }
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$stage/$asset")
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$stage/$asset")
    else
        echo "Install sha256sum or shasum to verify Herdr Omni." >&2
        exit 1
    fi
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || { echo "Herdr Omni checksum verification failed." >&2; exit 1; }
    mv "$stage/$asset" "$stage/herdr-omni"
fi
chmod 755 "$stage/herdr-omni"
reported=$("$stage/herdr-omni" --version)
[ "$reported" = "herdr-omni $version" ] || { echo "Herdr Omni binary version does not match the plugin." >&2; exit 1; }
mv -f "$stage/herdr-omni" "$root/bin/herdr-omni"
echo "Installed Herdr Omni v$version."
