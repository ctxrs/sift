#!/bin/sh
# Verify the binary and license notices before replacing installed files.
main() (
    set -eu

    fail() { printf 'retok: %s\n' "$*" >&2; exit 1; }
    case "$(uname -s):$(uname -m)" in
        Linux:x86_64|Linux:amd64) asset=retok-linux-x64 ;;
        Linux:aarch64|Linux:arm64) asset=retok-linux-aarch64 ;;
        Darwin:x86_64|Darwin:amd64) asset=retok-macos-x64 ;;
        Darwin:arm64|Darwin:aarch64) asset=retok-macos-arm64 ;;
        *) fail 'Unsupported platform. Expected Linux or macOS on x64 or ARM64.' ;;
    esac

    command -v curl >/dev/null 2>&1 || fail 'curl is required.'
    if command -v sha256sum >/dev/null 2>&1; then
        checksum=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum=shasum
    else
        fail 'sha256sum or shasum is required.'
    fi

    base=https://github.com/ctxrs/retok/releases
    version=${RETOK_VERSION:-}
    case "$version" in *[!A-Za-z0-9._-]*) fail 'Invalid RETOK_VERSION release tag.' ;; esac
    if [ -n "$version" ]; then
        base=$base/download/$version
    else
        base=$base/latest/download
    fi
    install_dir=${RETOK_INSTALL_DIR:-"$HOME/.local/bin"}
    # Absolute paths also keep leading dashes from becoming utility options.
    case "$install_dir" in /*) ;; *) install_dir=$PWD/$install_dir ;; esac
    notices=$asset.third-party-notices.txt
    mkdir -p "$install_dir"
    for target in retok retok.third-party-notices.txt; do
        [ ! -d "$install_dir/$target" ] || fail "$install_dir/$target is a directory."
    done
    temp_dir=$(mktemp -d "$install_dir/.retok.XXXXXX")
    trap 'rm -rf "$temp_dir"' 0
    trap 'exit 1' HUP INT TERM

    curl -fsSL --proto '=https' --tlsv1.2 "$base/SHA256SUMS" -o "$temp_dir/SHA256SUMS"
    for file in "$asset" "$notices"; do
        curl -fsSL --proto '=https' --tlsv1.2 "$base/$file" -o "$temp_dir/$file"
        expected=$(awk -v name="$file" 'NF == 2 && ($2 == name || $2 == "*" name) { print $1 }' "$temp_dir/SHA256SUMS")
        [ "${#expected}" -eq 64 ] || fail "Missing or ambiguous SHA-256 checksum for $file."
        case "$expected" in *[!0-9a-fA-F]*) fail "Invalid SHA-256 checksum for $file." ;; esac
        if [ "$checksum" = sha256sum ]; then
            actual=$(sha256sum "$temp_dir/$file")
        else
            actual=$(shasum -a 256 "$temp_dir/$file")
        fi
        actual=${actual%% *}
        expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
        [ "$actual" = "$expected" ] || fail "SHA-256 mismatch for $file."
    done

    chmod 755 "$temp_dir/$asset"
    chmod 644 "$temp_dir/$notices"
    # Same-filesystem renames avoid truncating an existing executable. Install
    # notices first so a new binary is never installed without its license text.
    notice_destination=$install_dir/retok.third-party-notices.txt
    notice_backup=$temp_dir/previous-notices
    if [ -e "$notice_destination" ]; then
        cp -p "$notice_destination" "$notice_backup"
    fi
    mv -f "$temp_dir/$notices" "$notice_destination"
    if ! mv -f "$temp_dir/$asset" "$install_dir/retok"; then
        if [ -e "$notice_backup" ]; then
            mv -f "$notice_backup" "$notice_destination" || {
                trap - 0
                fail "Executable replacement and notices rollback failed; backup retained at $notice_backup."
            }
        else
            rm -f "$notice_destination" || fail 'Executable replacement failed; could not remove new notices.'
        fi
        fail 'Executable replacement failed; previous notices state restored.'
    fi
    printf 'Installed retok to %s/retok\n' "$install_dir"
    printf 'Add %s to your PATH if needed, then run retok --help.\n' "$install_dir"
)

main
