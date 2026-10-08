# Fairspoken for Android (prototype)

Dictate into any app on the phone with your own Fairspoken transcription
host, reached over Tailscale. Audio streams to the host while you speak
(`POST /v1/transcriptions/stream`), so the text is back a moment after you
stop.

Two ways to use it in other apps:

- **Voice keyboard** (`ime/`): an input method with one big mic. Switch to it
  from any keyboard's switcher. It starts listening when it opens, commits at
  the cursor, and the globe key returns to your normal keyboard. It needs no
  extra permissions and is the safest route on Google Play.
- **Floating dock** (`dock/`, `insert/`): a frosted bubble over every app. It
  keeps your own keyboard: tap to start or stop, hold to talk while held, drag
  to move. An accessibility service puts the text in the focused field
  (`ACTION_SET_TEXT` at the cursor, or paste for rich editors) and shows the
  bubble only while a keyboard is open. Without the service the dock copies
  the text to the clipboard. This is the same design as Wispr Flow on Android.

The app screen finds a host by its Tailscale machine name, pairs with the
host's pairing password (`POST /v1/pair`), lets you try a dictation, and
picks a vocabulary pack (software engineering, GP practice). The pack's
always-on terms go to the host as `x-fairspoken-vocabulary-hints`.

## Build and run

Needs JDK 21 and an Android SDK with platform 37.

```sh
cd apps/android
export JAVA_HOME=/opt/homebrew/opt/openjdk@21
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
./gradlew assembleRelease
adb install -r app/build/outputs/apk/release/app-release.apk
```

The release build is minified and signed with the debug key so it can be
sideloaded. Use the release build to judge speed: Compose in a debug build is
several times slower.

Run a host the phone can reach, for example:

```sh
FAIRSPOKEN_HOST_ADDR=0.0.0.0:48173 FAIRSPOKEN_HOST_PAIRING_PASSWORD=<password> \
  src-tauri/target/release/transcription-host
```

Then connect with the host's Tailscale name or `100.x` address. The app
refuses plain HTTP to a LAN address (`192.168.…`), even though the host
answers there: use the tailnet address, or put the host behind HTTPS.

## Notes and limits

- **Finding hosts.** The Tailscale Android app does not share its peer list,
  so the phone can't scan the tailnet the way the desktop app does with
  `tailscale status --json`. You type the machine name once. The app expands
  it with the MagicDNS domain and probes the protocol's URLs in order.
- **Plain HTTP** is allowed only to tailnet addresses (`100.64.0.0/10`,
  `fd7a:115c:a1e0::/48`) and this device, because tailnet traffic is
  WireGuard-encrypted. `HostClient` checks the address it actually connected
  to before sending anything, since `network_security_config.xml` can't name
  address ranges. Hosts behind `tailscale serve` HTTPS are tried first.
- **The dock's microphone service** must be started from the Fairspoken
  screen. Android refuses to start microphone services from the background,
  so the service does not restart itself.
- **Sideloaded apps** need *App info › ⋮ › Allow restricted settings* before
  Android 13+ lets you turn on the accessibility service.
- The token is sealed with an AES-GCM key in the Android Keystore before it
  goes into private app preferences, and those preferences are left out of
  cloud backups and device transfers. Tokens saved by older builds are sealed
  on first launch.
