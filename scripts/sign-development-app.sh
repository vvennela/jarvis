#!/bin/bash
# Persistent, local-only identity. No trust or TCC settings are changed.
set -euo pipefail
umask 077
if (($# != 1)) || [[ ! -d "$1/Contents" || -L "$1" || -L "$1/Contents" ]]; then
  printf '%s\n' 'Usage: scripts/sign-development-app.sh APP_BUNDLE' >&2
  exit 2
fi
bundle="$1"
signing_dir="${JARVIS_SIGNING_DIR:-$HOME/Library/Application Support/Jarvis/Signing}"
if [[ "$signing_dir" != /* || -L "$signing_dir" ]]; then
  printf '%s\n' 'The signing directory must be an absolute, real directory.' >&2
  exit 2
fi
mkdir -p "$signing_dir"
chmod 700 "$signing_dir"
lock="$signing_dir/.lock"
if ! mkdir "$lock" 2>/dev/null; then
  printf '%s\n' 'Another Jarvis signing operation is active.' >&2
  exit 1
fi
work="$(mktemp -d "$signing_dir/prepare.XXXXXX")"
keychain="$signing_dir/development.keychain-db"
password_file="$signing_dir/keychain-password"
certificate="$signing_dir/development.cer"
trap '/usr/bin/security lock-keychain "$keychain" >/dev/null 2>&1 || true; rm -rf -- "$work"; rmdir "$lock"' EXIT

if [[ ! -e "$keychain" && ! -e "$password_file" && ! -e "$certificate" ]]; then
  /usr/bin/openssl rand -hex 32 > "$password_file"
  password="$(cat "$password_file")"
  cat > "$work/certificate.conf" <<'CONFIG'
[req]
distinguished_name = subject
x509_extensions = extensions
prompt = no
[subject]
CN = Jarvis Local Development
[extensions]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
CONFIG
  /usr/bin/openssl req -new -newkey rsa:2048 -nodes -x509 -days 3650 \
    -config "$work/certificate.conf" -keyout "$work/key.pem" \
    -out "$work/certificate.pem" 2> "$work/openssl.log"
  /usr/bin/openssl x509 -in "$work/certificate.pem" -outform DER -out "$certificate"
  /usr/bin/openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/certificate.pem" \
    -name 'Jarvis Local Development' -out "$work/identity.p12" -passout "file:$password_file"
  /usr/bin/security create-keychain -p "$password" "$keychain" >/dev/null
  /usr/bin/security set-keychain-settings -lut 300 "$keychain"
  /usr/bin/security unlock-keychain -p "$password" "$keychain"
  /usr/bin/security import "$work/identity.p12" -k "$keychain" -P "$password" \
    -x -T /usr/bin/codesign >/dev/null
  # This isolated keychain contains only Jarvis's local signing key.
  /usr/bin/security set-key-partition-list -S apple-tool: -s -k "$password" "$keychain" >/dev/null
fi
for path in "$keychain" "$password_file" "$certificate"; do
  if [[ ! -f "$path" || -L "$path" ]]; then
    printf '%s\n' 'Jarvis signing identity is incomplete; preserve it for repair instead of rotating permissions.' >&2
    exit 1
  fi
done
password="$(cat "$password_file")"
/usr/bin/security unlock-keychain -p "$password" "$keychain"
fingerprint="$(/usr/bin/openssl x509 -inform DER -in "$certificate" -noout -fingerprint -sha1 | /usr/bin/sed 's/.*=//; s/://g')"
requirement="designated => identifier \"com.jarvis.panel\" and certificate leaf = H\"$fingerprint\""
# codesign's identity lookup also requires the keychain in the search list,
# even with --keychain. Restore only our temporary entry after signing.
/usr/bin/python3 - "$keychain" "$fingerprint" "$requirement" "$bundle" <<'PY'
import shlex
import subprocess
import sys

keychain, fingerprint, requirement, bundle = sys.argv[1:]
security = "/usr/bin/security"
def search_list():
    return shlex.split(subprocess.check_output([security, "list-keychains", "-d", "user"], text=True))
original = search_list()
added = keychain not in original
try:
    if added:
        subprocess.run([security, "list-keychains", "-d", "user", "-s", *original, keychain], check=True)
    subprocess.run(["/usr/bin/codesign", "--force", "--timestamp=none", "--keychain", keychain,
                    "--sign", fingerprint, "--identifier", "com.jarvis.panel",
                    "--requirements", "=" + requirement, bundle], check=True)
finally:
    if added:
        current = [path for path in search_list() if path != keychain]
        subprocess.run([security, "list-keychains", "-d", "user", "-s", *current], check=True)
PY
/usr/bin/codesign --verify --strict "$bundle"
/usr/bin/security lock-keychain "$keychain"
