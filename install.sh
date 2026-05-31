#!/bin/sh
# brainlog installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/urjitbhatia/brainlog/master/install.sh | sh
#
# Environment variables:
#   BRAINLOG_VERSION      Version to install (e.g. 0.4.0). Defaults to the latest release.
#   BRAINLOG_INSTALL_DIR  Directory to install into. Defaults to ~/.local/bin.

set -eu

REPO="urjitbhatia/brainlog"
BIN="brainlog"

# ---- helpers ---------------------------------------------------------------

err() {
	printf 'error: %s\n' "$1" >&2
	exit 1
}

info() {
	printf '%s\n' "$1" >&2
}

need() {
	command -v "$1" >/dev/null 2>&1 || err "required command '$1' not found"
}

# Download URL to stdout. Prefers curl, falls back to wget.
fetch() {
	url="$1"
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL "$url"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO- "$url"
	else
		err "need either 'curl' or 'wget' to download files"
	fi
}

# Download URL to a file.
fetch_to() {
	url="$1"
	dest="$2"
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL "$url" -o "$dest"
	else
		wget -qO "$dest" "$url"
	fi
}

# ---- detect platform -------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
	Darwin)
		case "$arch" in
			arm64 | aarch64) target="aarch64-apple-darwin" ;;
			x86_64 | amd64) target="x86_64-apple-darwin" ;;
			*) err "no prebuilt binary for macOS $arch. Build from source: cargo install --git https://github.com/$REPO" ;;
		esac
		;;
	Linux)
		# Prefer the statically-linked musl binaries: they run on any distro
		# (including Alpine and older glibc) with no shared-library dependencies.
		case "$arch" in
			x86_64 | amd64) target="x86_64-unknown-linux-musl" ;;
			aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
			*) err "no prebuilt binary for Linux $arch. Build from source: cargo install --git https://github.com/$REPO" ;;
		esac
		;;
	*)
		err "unsupported OS '$os'. brainlog supports macOS and Linux."
		;;
esac

# ---- resolve version -------------------------------------------------------

version="${BRAINLOG_VERSION:-}"
if [ -z "$version" ]; then
	info "Resolving latest release..."
	# Parse tag_name from the GitHub releases API without requiring jq.
	tag="$(fetch "https://api.github.com/repos/$REPO/releases/latest" |
		grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name"[^"]*"([^"]+)".*/\1/')"
	[ -n "$tag" ] || err "could not determine latest release. Set BRAINLOG_VERSION explicitly."
	version="${tag#v}"
fi

archive="${BIN}-${target}-v${version}.tar.gz"
base_url="https://github.com/$REPO/releases/download/v${version}"

# ---- install dir -----------------------------------------------------------

install_dir="${BRAINLOG_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$install_dir"

# ---- download, verify, extract --------------------------------------------

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

info "Downloading $archive (v${version}, ${target})..."
fetch_to "$base_url/$archive" "$tmp/$archive" ||
	err "failed to download $archive — does release v${version} include the $target target?"

# Verify checksum if a .sha256 is published and a checker is available.
if fetch_to "$base_url/$archive.sha256" "$tmp/$archive.sha256" 2>/dev/null; then
	expected="$(awk '{print $1}' "$tmp/$archive.sha256")"
	if command -v sha256sum >/dev/null 2>&1; then
		actual="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
	elif command -v shasum >/dev/null 2>&1; then
		actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
	else
		actual=""
	fi
	if [ -n "$actual" ] && [ "$expected" != "$actual" ]; then
		err "checksum mismatch for $archive (expected $expected, got $actual)"
	fi
	[ -n "$actual" ] && info "Checksum verified."
fi

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/$BIN" ] || err "archive did not contain expected binary '$BIN'"

install_path="$install_dir/$BIN"
mv "$tmp/$BIN" "$install_path"
chmod +x "$install_path"

info ""
info "Installed brainlog v${version} to $install_path"

# ---- PATH hint -------------------------------------------------------------

case ":$PATH:" in
	*":$install_dir:"*) ;;
	*)
		info ""
		info "Note: $install_dir is not on your PATH. Add it, e.g.:"
		info "  export PATH=\"$install_dir:\$PATH\""
		;;
esac

info ""
"$install_path" --version 2>/dev/null || true
