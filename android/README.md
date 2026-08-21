# LouiseLM Android capture

The Android app is a small offline-first recorder, not a workflow UI. One tap
starts speech capture; stopping atomically publishes immutable audio and
metadata in app-private storage. Pairing and upload can happen later.

## Build

Install Android SDK Platform 36, Build Tools 36.0.0, and Platform Tools. Gradle
must be able to find that SDK through `ANDROID_HOME` or an ignored
`local.properties` file in this directory:

```properties
sdk.dir=/absolute/path/to/Android/Sdk
```

Connect an API 28+ device, then run:

```sh
./gradlew test lint assembleDebug
./gradlew installDebug
```

The app uses the system camera to take a full-resolution image of the terminal
QR and bundled ML Kit to decode it offline. Pairing and uploads use HTTPS with
the stable receiver public-key identity carried by the version-2 one-time QR;
routine TLS certificate renewal with that key remains trusted. The long-lived
device credential and receiver identity are encrypted by Android Keystore.
WorkManager retries only network, timeout, rate-limit, and server failures.

Configure one private receiver profile before running
`:LouiselmCapturePair`; the receiver refuses pairing while it is loopback-only.
The Android app does not support version-1 exact-certificate pairing offers.

## Physical acceptance checklist

- Record while offline, stop, force-stop the app, reopen it, and confirm the
  capture remains queued.
- Start recording, background the app, and confirm `onStop` saves the capture.
- Pair from `:LouiselmCapturePair`, then inspect the same UUID in
  `:LouiselmInbox`.
- Interrupt an upload, reconnect, and confirm the receiver contains one capture
  with that UUID rather than duplicates.
- Reboot the phone with queued work and confirm WorkManager eventually uploads
  after connectivity returns.
- Revoke the device with `louiselm-capture revoke-device`, record again, and
  confirm upload needs operator attention while the original remains local.
- Renew the receiver certificate with the same key and confirm uploads continue;
  replace the receiver key and confirm the app rejects the new identity.

Automated tests never use a real microphone, receiver, credential, or network.
