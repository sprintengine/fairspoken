#!/usr/bin/env bash
# Pick a STABLE code-signing identity for local builds and write Config/Local.xcconfig.
#
# Why: macOS keys Accessibility / Input Monitoring grants on the code signature. Ad-hoc
# signatures change on every build, so the grant silently stops working after a rebuild.
# Any certificate-backed identity keeps the designated requirement stable across rebuilds.
#
# Preference: $FS_SIGN_IDENTITY, then "Apple Development", then "Developer ID Application",
# then a self-signed "Fairspoken Local Signing" certificate created in the login keychain.
set -euo pipefail
cd "$(dirname "$0")/.."
pick() { security find-identity -v -p codesigning | grep -F "$1" | head -1 | sed -E 's/^ *[0-9]+\) [0-9A-F]+ "(.*)"$/\1/'; }
ID="${FS_SIGN_IDENTITY:-}"
[[ -z "$ID" ]] && ID="$(pick 'Apple Development:')"
[[ -z "$ID" ]] && ID="$(pick 'Developer ID Application:')"
if [[ -z "$ID" ]]; then
  NAME="Fairspoken Local Signing"
  if ! security find-certificate -c "$NAME" >/dev/null 2>&1; then
    echo "Creating self-signed code-signing certificate '$NAME' (login keychain)…"
    TMP=$(mktemp -d)
    cat > "$TMP/cfg" <<CFG
[req]
distinguished_name=dn
x509_extensions=ext
prompt=no
[dn]
CN=$NAME
[ext]
keyUsage=critical,digitalSignature
extendedKeyUsage=critical,codeSigning
basicConstraints=critical,CA:false
CFG
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -config "$TMP/cfg" >/dev/null 2>&1
    openssl pkcs12 -export -legacy -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -out "$TMP/id.p12" -passout pass:fairspoken >/dev/null 2>&1 \
      || openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -out "$TMP/id.p12" -passout pass:fairspoken
    security import "$TMP/id.p12" -k ~/Library/Keychains/login.keychain-db -P fairspoken -T /usr/bin/codesign
    echo "Now trust it for code signing: Keychain Access › '$NAME' › Trust › Code Signing: Always Trust (one-time, needs your password)."
    rm -rf "$TMP"
  fi
  ID="$NAME"
fi
# Team = the certificate's OU (the "(XXXXXXXXXX)" in an Apple Development name is NOT the team).
TEAM=$(security find-certificate -c "$ID" -p 2>/dev/null | openssl x509 -noout -subject 2>/dev/null | sed -nE 's#.*/OU=([A-Z0-9]{10}).*#\1#p' || true)
cat > Config/Local.xcconfig <<CFG
// Written by scripts/setup-signing.sh — git-ignored, machine-specific.
MV_SIGN_IDENTITY = $ID
DEVELOPMENT_TEAM = $TEAM
CFG
echo "Signing identity: $ID ${TEAM:+(team $TEAM)}"
