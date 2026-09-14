#!/bin/sh
# Install an official OvpnLane release. Configuration files are never modified.
set -eu

release_root=https://github.com/debba/ovpnlane/releases
requested_version=${OVPNLANE_VERSION:-latest}
install_dir=${OVPNLANE_INSTALL_DIR:-"$HOME/.local/bin"}

die() {
    printf 'ovpnlane installer: %s\n' "$*" >&2
    exit 1
}

for tool in curl uname tar awk grep mktemp mkdir chmod mv; do
    command -v "$tool" >/dev/null 2>&1 || die "missing required command: $tool"
done

case "$(uname -s):$(uname -m)" in
    Darwin:arm64 | Darwin:aarch64) platform=macos-arm64 ;;
    Darwin:x86_64) platform=macos-x86_64 ;;
    Linux:x86_64 | Linux:amd64) platform=linux-x86_64 ;;
    *) die 'no prebuilt archive for this system; see the README for source builds and Windows downloads' ;;
esac
if [ "$platform" = linux-x86_64 ]; then
    if [ -e /etc/alpine-release ] || { command -v ldd >/dev/null 2>&1 && ldd --version 2>&1 | grep -qi musl; }; then
        die 'this release requires glibc; build from source on musl/Alpine'
    fi
fi

fetch() {
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 --fail --location \
        --silent --show-error --connect-timeout 15 --max-time 180 "$@"
}

if [ "$requested_version" = latest ]; then
    resolved_url=$(fetch -o /dev/null -w '%{url_effective}' "$release_root/latest")
    case "$resolved_url" in
        "$release_root/tag/v"*) tag=${resolved_url##*/} ;;
        *) die 'could not resolve the latest GitHub release' ;;
    esac
else
    tag=v${requested_version#v}
fi
version=${tag#v}
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' \
    || die 'invalid release version; use a version such as 0.0.1'

archive=ovpnlane-${version}-${platform}.tar.gz
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/ovpnlane-install.XXXXXXXX")
staged_binary=
cleanup() {
    rm -rf "$work_dir"
    if [ -n "$staged_binary" ]; then rm -f "$staged_binary"; fi
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

printf 'Downloading OvpnLane %s for %s...\n' "$version" "$platform"
fetch "$release_root/download/$tag/$archive" -o "$work_dir/archive.tar.gz"
fetch "$release_root/download/$tag/$archive.sha256" -o "$work_dir/checksum"
expected=$(awk -v name="$archive" 'NR == 1 && NF == 2 && $2 == name { print $1 }' "$work_dir/checksum")
printf '%s\n' "$expected" | grep -Eq '^[0-9a-fA-F]{64}$' || die 'invalid SHA-256 manifest'
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work_dir/archive.tar.gz" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$work_dir/archive.tar.gz" | awk '{print $1}')
else
    die 'sha256sum or shasum is required'
fi
[ "$expected" = "$actual" ] || die 'SHA-256 checksum mismatch; installation was not changed'

# Extract only the named binary, inside a private temporary directory.
tar -xzf "$work_dir/archive.tar.gz" -C "$work_dir" ovpnlane
[ -f "$work_dir/ovpnlane" ] && [ ! -L "$work_dir/ovpnlane" ] || die 'archive does not contain a regular ovpnlane executable'
chmod 755 "$work_dir/ovpnlane"
reported_version=$("$work_dir/ovpnlane" --version) || die 'binary cannot run on this system; see platform requirements in README'
[ "$reported_version" = "ovpnlane $version" ] || die 'binary version does not match the selected release'

mkdir -p "$install_dir" || die "cannot create $install_dir; choose a writable OVPNLANE_INSTALL_DIR"
[ -w "$install_dir" ] || die "cannot write to $install_dir"
[ ! -L "$install_dir/ovpnlane" ] || die 'installation target is a symlink; choose a directory containing a regular executable'
[ ! -d "$install_dir/ovpnlane" ] || die 'installation target is a directory'
staged_binary=$(mktemp "$install_dir/.ovpnlane-install.XXXXXXXX")
cat "$work_dir/ovpnlane" > "$staged_binary"
chmod 755 "$staged_binary"
mv -f "$staged_binary" "$install_dir/ovpnlane"
staged_binary=
printf 'Installed %s\n' "$install_dir/ovpnlane"
printf 'Check for updates: ovpnlane update --check\n'
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) printf 'Add this directory to PATH: %s\n' "$install_dir" ;;
esac
