#!/bin/bash
# Builds a native development bundle, then launches it from the project so .env
# stays in one place. The app executable is a real Mach-O binary, not a shell.
set -euo pipefail

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
invocation_dir="$PWD"
root_dir="$repo_dir/examples/demo-workspace"
database_path="$repo_dir/runtime/context.sqlite"
build_only=false
smoke_test=false

usage() {
  printf '%s\n' 'Usage: scripts/launch-jarvis.command [--root DIRECTORY] [--db FILE] [--build-only] [--smoke-test]'
}
while (($#)); do
  case "$1" in
    --root)
      if (($# < 2)); then usage >&2; exit 2; fi
      root_dir="$2"; shift 2 ;;
    --db)
      if (($# < 2)); then usage >&2; exit 2; fi
      database_path="$2"; shift 2 ;;
    --build-only) build_only=true; shift ;;
    --smoke-test) smoke_test=true; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
if [[ "$(uname -s)" != Darwin ]]; then
  printf '%s\n' 'The Jarvis native launcher runs on macOS.' >&2
  exit 1
fi
if [[ -z "$root_dir" || ! -d "$root_dir" || -L "$root_dir" || -z "$database_path" ]]; then
  printf '%s\n' 'Choose an existing real directory for --root and a file path for --db.' >&2
  exit 2
fi
[[ "$root_dir" == /* ]] || root_dir="$invocation_dir/$root_dir"
[[ "$database_path" == /* ]] || database_path="$invocation_dir/$database_path"

# Finder's shell environment may omit Homebrew and rustup's binary directories.
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$PATH"
cd -- "$repo_dir"
cargo build --manifest-path "$repo_dir/Cargo.toml" --target-dir "$repo_dir/target" -p jarvis-panel
mkdir -p "$repo_dir/runtime"
# Documents can be managed by a file provider that adds Finder/resource metadata
# after signing. Keep the native executable in local Application Support instead.
app_dir="$HOME/Library/Application Support/Jarvis/Development"
mkdir -p "$app_dir"
staging_dir="$(mktemp -d "$app_dir/jarvis-package.XXXXXX")"
trap 'rm -rf -- "$staging_dir"' EXIT
staging_app="$staging_dir/Jarvis.app"
mkdir -p "$staging_app/Contents/MacOS"
cp -X "$repo_dir/target/debug/jarvis-panel" "$staging_app/Contents/MacOS/jarvis-panel"
cp -X "$repo_dir/crates/jarvis-panel/Info.plist" "$staging_app/Contents/Info.plist"
chmod 755 "$staging_app/Contents/MacOS/jarvis-panel"
/usr/bin/plutil -lint "$staging_app/Contents/Info.plist" >/dev/null
/usr/bin/xattr -cr "$staging_app"
/usr/bin/codesign --force --sign - --identifier com.jarvis.panel "$staging_app"
/usr/bin/codesign --verify --strict "$staging_app"

bundle_path="$app_dir/Jarvis.app"
if [[ -L "$bundle_path" ]]; then
  printf '%s\n' 'The development bundle destination must be a real directory.' >&2
  exit 1
fi
if [[ -e "$bundle_path" ]]; then
  existing_id="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$bundle_path/Contents/Info.plist" 2>/dev/null || true)"
  if [[ "$existing_id" != com.jarvis.panel ]]; then
    printf '%s\n' 'The development destination already contains a different application.' >&2
    exit 1
  fi
fi
# Only the generated Jarvis bundle is replaced; credentials stay in project .env.
rm -rf -- "$bundle_path"
mv -- "$staging_app" "$bundle_path"
/usr/bin/codesign --verify --strict "$bundle_path"
# Preserve the repository entry point while keeping signed files off the provider.
runtime_link="$repo_dir/runtime/Jarvis.app"
if [[ -L "$runtime_link" ]]; then
  rm -- "$runtime_link"
elif [[ -e "$runtime_link" ]]; then
  existing_id="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$runtime_link/Contents/Info.plist" 2>/dev/null || true)"
  if [[ "$existing_id" != com.jarvis.panel ]]; then
    printf '%s\n' 'The runtime destination already contains a different application.' >&2
    exit 1
  fi
  rm -rf -- "$runtime_link"
fi
ln -s "$bundle_path" "$runtime_link"
/usr/bin/codesign --verify --strict "$runtime_link"
printf 'Jarvis app ready: %s\n' "$bundle_path"
if "$build_only"; then exit 0; fi

launch_args=(--root "$root_dir" --db "$database_path" --project-dir "$repo_dir")
if "$smoke_test"; then
  # Tests need bounded foreground execution and structured stdout; no mic/OS reads.
  "$bundle_path/Contents/MacOS/jarvis-panel" "${launch_args[@]}" --smoke-test
else
  # LaunchServices owns the native app lifetime, independent of the launcher shell.
  /usr/bin/open -n "$bundle_path" --args "${launch_args[@]}"
fi
