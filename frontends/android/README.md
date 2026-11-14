# oxi for Android

A native client (Kotlin, Jetpack Compose) for `oxi server`: browse projects, chat
with the server's agents, approve tool calls, switch agents and models. Setup for
users is in [docs/server.md](../../docs/server.md).

It speaks the same JSON-lines protocol as the Sublime Text package
(`src/headless/mod.rs`) over TCP to the daemon's phone listener
(`src/headless/phone.rs`), which accepts Tailscale peers carrying the pairing token.

## What it does

- **Chats**: streaming replies, tool calls with diffs, the agent's checklist above the
  composer, plan mode, evidence mode, model and agent picker, context use.
- **Attachments**: take a photo with the camera, pick photos and files, or share text,
  links, photos and files into oxi from any other app. Drafts are kept per chat.
- **Questions and approvals**: tool approvals, and the agent's questions
  (`AskUserQuestion`, MCP forms, sign-in pages) as a form to fill.
- **In the background**: while a chat the phone sent a prompt to is running,
  `TurnService` (a foreground service) keeps the connection up. A notification tells
  when the reply is ready, with a Reply field for the follow-up, or when a tool waits for
  approval, with Allow (needs the phone unlocked) and Deny.
- **Source control**: changed files and their diffs, stage, commit (the server's model
  can write the message), pull, push, recent commits.

The connection belongs to the process (`OxiApp` in `Core.kt`), not to a screen, so the
service and the notification actions (`Background.kt`) use it without an activity.

## Build

Needs JDK 17 and the Android SDK (platform 35). With Homebrew:

```sh
brew install openjdk@17 && brew install --cask android-commandlinetools
sdkmanager "platform-tools" "platforms;android-35" "build-tools;35.0.0"
echo "sdk.dir=$(brew --prefix)/share/android-commandlinetools" > local.properties
JAVA_HOME=$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home ./gradlew assembleRelease
adb install -r app/build/outputs/apk/release/app-release.apk
```

The version comes from the workspace `Cargo.toml`, so the app and oxi release
together.

## Signing and updates

The app updates itself from GitHub releases (`Updater.kt`): it downloads
`oxi-android.apk`, checks it against `SHA256SUMS` and installs it with
PackageInstaller. Android only accepts an update signed with the same key, so every
build that may reach a phone is signed with the release key:

- **Release CI** (`.github/workflows/release.yml`, job `android`) reads it from the
  secrets `OXI_ANDROID_KEYSTORE_B64` (the base64 keystore) and
  `OXI_ANDROID_KEYSTORE_PASSWORD`. Without them the release goes out without the APK
  (with a warning) rather than signing it with another key.
- **Locally**, `~/.config/oxi-android/keystore.properties` (`storeFile`,
  `storePassword`, `keyAlias`, `keyPassword`). Without it builds use the debug key;
  they install, but can't update a release-signed app.

Setting the secrets from the keystore on a developer machine:

```sh
base64 -i ~/.config/oxi-android/release.jks | gh secret set OXI_ANDROID_KEYSTORE_B64
grep storePassword ~/.config/oxi-android/keystore.properties | cut -d= -f2 | tr -d '\n' \
  | gh secret set OXI_ANDROID_KEYSTORE_PASSWORD
```

Keep a backup of the keystore: if it is lost, installed apps can't update and have
to be reinstalled.

## Testing an update without a release

`-PoxiVersion=X.Y.Z` overrides the version and `-PoxiUpdateFeed=URL` points the
updater at another feed laid out like GitHub's: `URL/latest` (`{"tag_name": "vX.Y.Z",
"assets": [{"name": "oxi-android.apk"}]}`) and `URL/download/vX.Y.Z/{oxi-android.apk,SHA256SUMS}`.

```sh
./gradlew assembleRelease                      # the "new" version, into the feed folder
./gradlew assembleRelease -PoxiVersion=1.0.0 -PoxiUpdateFeed=http://127.0.0.1:8765
python3 -m http.server 8765 --bind 127.0.0.1 --directory feed
adb reverse tcp:8765 tcp:8765                  # the phone's 127.0.0.1:8765 is this machine's
```

Install the old build, open the app: the banner offers the new version. Android asks
once to confirm an update from an app it didn't install; after that oxi is the
installer of record and later updates go through without asking.
