#!/usr/bin/env bash
set -euo pipefail

output=${1:-}
if [[ -z "$output" || $# -ne 1 ]]; then
  printf 'Usage: build-macos-release.sh <output-directory>\n' >&2
  exit 2
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
  printf 'macOS release builds require an Apple Silicon Mac.\n' >&2
  exit 1
fi

for command in bun cargo security codesign xcrun python3 hdiutil spctl shasum ditto plutil; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'Missing macOS release command: %s\n' "$command" >&2
    exit 1
  }
done
for tool in notarytool stapler; do
  xcrun --find "$tool" >/dev/null 2>&1 || {
    printf 'Missing macOS release tool: %s\n' "$tool" >&2
    exit 1
  }
done

for variable in APPLE_SIGNING_IDENTITY APPLE_KEYCHAIN APPLE_KEYCHAIN_PASSWORD APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH; do
  if [[ -z ${!variable:-} ]]; then
    printf 'Missing macOS release variable: %s\n' "$variable" >&2
    exit 1
  fi
done
[[ -f "$APPLE_KEYCHAIN" ]] || { printf 'APPLE_KEYCHAIN is not a file.\n' >&2; exit 1; }
[[ -r "$APPLE_API_KEY_PATH" ]] || { printf 'APPLE_API_KEY_PATH is not readable.\n' >&2; exit 1; }
[[ "$APPLE_SIGNING_IDENTITY" == 'Developer ID Application:'* ]] || {
  printf 'APPLE_SIGNING_IDENTITY must be a Developer ID Application identity.\n' >&2
  exit 1
}

developer_dir=$(xcode-select -p 2>/dev/null || true)
[[ -d "$developer_dir" ]] || {
  printf 'No valid Apple developer toolchain is selected.\n' >&2
  exit 1
}
foundation_framework=$(xcrun --sdk macosx --show-sdk-path)/System/Library/Frameworks/FoundationModels.framework
[[ -d "$foundation_framework" ]] || {
  printf 'The selected Xcode SDK does not contain FoundationModels.framework.\n' >&2
  exit 1
}
find "$developer_dir" -iname '*FoundationModelsMacros*' -print -quit | grep -q . || {
  printf 'The selected developer toolchain does not contain the Foundation Models macro plugin.\n' >&2
  exit 1
}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
version=$(python3 -c 'import json; print(json.load(open("package.json"))["version"])')
tauri_version=$(python3 -c 'import json; print(json.load(open("src-tauri/tauri.conf.json"))["version"])')
cargo_version=$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' src-tauri/Cargo.toml | head -1)
[[ "$version" == "$tauri_version" && "$version" == "$cargo_version" ]] || {
  printf 'MotsDits package, Tauri, and Cargo versions must match.\n' >&2
  exit 1
}
source_commit=${MOTSDITS_SOURCE_COMMIT:-}
if [[ ! "$source_commit" =~ ^[0-9a-f]{40}$ ]]; then
  source_commit=$(git rev-parse HEAD 2>/dev/null || true)
fi
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || { printf 'Missing exact source commit.\n' >&2; exit 1; }
[[ ! -e "$output" && ! -L "$output" ]] || { printf 'Release output already exists: %s\n' "$output" >&2; exit 1; }

original_keychains=$(security list-keychains -d user | tr -d ' "')
release_keychain="$APPLE_KEYCHAIN"
cleanup_keychains() {
  if [[ -n "$original_keychains" ]]; then
    # shellcheck disable=SC2086
    security list-keychains -d user -s $original_keychains >/dev/null 2>&1 || true
  fi
}
trap cleanup_keychains EXIT
security unlock-keychain -p "$APPLE_KEYCHAIN_PASSWORD" "$release_keychain"
security set-keychain-settings "$release_keychain"
other_keychains=$(printf '%s\n' "$original_keychains" | grep -vxF "$release_keychain" || true)
# shellcheck disable=SC2086
security list-keychains -d user -s "$release_keychain" $other_keychains
identity_details=$(security find-identity -v -p codesigning "$release_keychain")
grep -Fq "\"$APPLE_SIGNING_IDENTITY\"" <<<"$identity_details" || {
  printf 'Selected Developer ID identity is unavailable in the release keychain.\n' >&2
  exit 1
}

output_parent=$(dirname "$output")
mkdir -p "$output_parent"
output_parent=$(cd "$output_parent" && pwd)
output="$output_parent/$(basename "$output")"
build_target=$(mktemp -d "$output_parent/.motsdits-macos-target.XXXXXX")
staging=$(mktemp -d "$output_parent/.motsdits-macos-release.XXXXXX")
work=$(mktemp -d "${TMPDIR:-/tmp}/motsdits-notary.XXXXXX")
release_config="$work/tauri.release.conf.json"
cleanup() {
  rm -rf "$build_target" "$staging" "$work"
  cleanup_keychains
}
trap cleanup EXIT

release_entitlements="$work/Entitlements.plist"
tr -d '\r' <src-tauri/Entitlements.plist >"$release_entitlements"
plutil -lint "$release_entitlements" >/dev/null
SIGNING_IDENTITY="$APPLE_SIGNING_IDENTITY" ENTITLEMENTS="$release_entitlements" python3 - <<'PY' >"$release_config"
import json
import os
print(json.dumps({
    "bundle": {
        "targets": ["app"],
        "macOS": {
            "signingIdentity": os.environ["SIGNING_IDENTITY"],
            "entitlements": os.environ["ENTITLEMENTS"],
        },
    }
}))
PY

notary_key="$APPLE_API_KEY"
notary_issuer="$APPLE_API_ISSUER"
notary_key_path="$APPLE_API_KEY_PATH"
unset APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH
export OTHER_CODE_SIGN_FLAGS="--keychain $release_keychain"
build_log="$work/tauri-build.log"
if ! CARGO_TARGET_DIR="$build_target" CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  MOTSDITS_REQUIRE_APPLE_INTELLIGENCE=1 \
  bunx tauri build --target aarch64-apple-darwin --bundles app --config "$release_config" \
  2>&1 | tee "$build_log"; then
  printf 'macOS Tauri release build failed.\n' >&2
  exit 1
fi
if ! grep -Fq 'Building with Apple Intelligence support.' "$build_log"; then
  printf 'Release build did not compile Apple Intelligence support.\n' >&2
  exit 1
fi
if grep -Fq 'Building with stubs.' "$build_log"; then
  printf 'Release build unexpectedly used Apple Intelligence stubs.\n' >&2
  exit 1
fi
export APPLE_API_KEY="$notary_key"
export APPLE_API_ISSUER="$notary_issuer"
export APPLE_API_KEY_PATH="$notary_key_path"

app="$build_target/aarch64-apple-darwin/release/bundle/macos/MotsDits.app"
[[ -d "$app" ]] || { printf 'MotsDits.app was not produced.\n' >&2; exit 1; }
codesign --verify --deep --strict --verbose=2 "$app"
app_details=$(codesign -dv --verbose=4 "$app" 2>&1)
grep -Fq 'Authority=Developer ID Application:' <<<"$app_details"
grep -Eq '^TeamIdentifier=[A-Z0-9]+$' <<<"$app_details"
grep -Eq '^Timestamp=' <<<"$app_details"

app_upload="$work/MotsDits.zip"
ditto -c -k --keepParent "$app" "$app_upload"
xcrun notarytool submit "$app_upload" \
  --key "$notary_key_path" --key-id "$notary_key" --issuer "$notary_issuer" --wait
xcrun stapler staple "$app"
xcrun stapler validate "$app"
spctl -a -vv -t exec "$app"

volume="$work/volume"
mkdir -p "$volume"
ditto "$app" "$volume/MotsDits.app"
ln -s /Applications "$volume/Applications"
dmg="$staging/MotsDits-macOS-AppleSilicon.dmg"
hdiutil create -volname MotsDits -srcfolder "$volume" -ov -format UDZO "$dmg"
codesign --force --sign "$APPLE_SIGNING_IDENTITY" --keychain "$release_keychain" --timestamp "$dmg"
xcrun notarytool submit "$dmg" \
  --key "$notary_key_path" --key-id "$notary_key" --issuer "$notary_issuer" --wait
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
spctl -a -vv -t open --context context:primary-signature "$dmg"

mountpoint="$work/mount"
mkdir -p "$mountpoint"
hdiutil attach -nobrowse -readonly -mountpoint "$mountpoint" "$dmg" >/dev/null
mounted=1
finish_mount() {
  if [[ ${mounted:-0} -eq 1 ]]; then hdiutil detach "$mountpoint" >/dev/null; fi
}
trap 'finish_mount; cleanup' EXIT
mounted_app="$mountpoint/MotsDits.app"
[[ -d "$mounted_app" ]] || { printf 'MotsDits.app is missing from the DMG.\n' >&2; exit 1; }
actual_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$mounted_app/Contents/Info.plist")
bundle_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$mounted_app/Contents/Info.plist")
[[ "$actual_version" == "$version" ]] || { printf 'macOS app version mismatch.\n' >&2; exit 1; }
[[ "$bundle_id" == 'tools.madera.motsdits' ]] || { printf 'macOS bundle identifier mismatch.\n' >&2; exit 1; }
codesign --verify --deep --strict "$mounted_app"
xcrun stapler validate "$mounted_app"
spctl -a -vv -t exec "$mounted_app"
finish_mount
mounted=0

sha=$(shasum -a 256 "$dmg" | cut -d ' ' -f 1)
team_id=$(codesign -dv --verbose=4 "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p' | head -1)
VERSION="$version" SOURCE_COMMIT="$source_commit" SHA="$sha" TEAM_ID="$team_id" DMG="$dmg" python3 - <<'PY' >"$staging/MotsDits-macOS-verification.json"
import json
import os
from pathlib import Path
artifact = Path(os.environ["DMG"])
print(json.dumps({
    "productName": "MotsDits",
    "version": os.environ["VERSION"],
    "platform": "macos",
    "architecture": "aarch64",
    "bundleIdentifier": "tools.madera.motsdits",
    "sourceCommit": os.environ["SOURCE_COMMIT"],
    "teamIdentifier": os.environ["TEAM_ID"],
    "developerIdVerified": True,
    "staplingVerified": True,
    "gatekeeperVerified": True,
    "artifact": {
        "name": artifact.name,
        "bytes": artifact.stat().st_size,
        "sha256": os.environ["SHA"],
    },
}, indent=2))
PY

mv "$staging" "$output"
rm -rf "$build_target" "$work"
build_target=''
staging=''
work=''
cleanup_keychains
trap - EXIT
printf 'Signed and notarized MotsDits %s macOS release staged at %s\n' "$version" "$output"
