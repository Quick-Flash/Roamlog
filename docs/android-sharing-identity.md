# Android sharing identity

`sharing::identity::load_android(&AndroidApp)` loads or creates an Iroh identity
when built with `ui` and `sharing` on Android. `load_context` accepts an attached
JNI environment and Android Context when no Zgui activity exists. Call from a
worker thread; Keystore, file locking, and storage operations block. Loading an identity
does not start an endpoint, request location access, or enable sharing.

Java bridge `dev.roamlog.android.SharingIdentity.loadOrCreate(Context)` stores a
32-byte random seed under `getNoBackupFilesDir()/sharing/identity.v1`. An AES-256
key with alias `roamlog.sharing.identity.v1` stays in Android Keystore. Cipher uses
AES-GCM, a provider-generated 12-byte IV, 128-bit authentication tag, and fixed
version-specific additional authenticated data. Keystore protection does not
promise hardware-backed storage on every device. Android's documented key
configuration is in [KeyGenParameterSpec](https://developer.android.com/reference/android/security/keystore/KeyGenParameterSpec).

Record format: version byte `1`, 12-byte IV, 48-byte encrypted seed plus tag.
Reads require exactly 61 bytes. AtomicFile writes, explicit fsync, and read-back
verification finish before a newly generated identity is returned. A Java monitor
serializes threads; a file lock serializes app processes. Seed bytes enter Rust
memory to construct Iroh's signing key. Disk never stores the plaintext seed.

Invalid versions, truncation, tampering, missing wrapping keys, and existing
Keystore entries without ciphertext fail closed. Existing identity data remains
untouched except for AtomicFile's backup recovery. An interrupted first creation
after Keystore generation can also require explicit reset. No automatic rotation
occurs. No reset or recovery UI exists yet. A future explicit identity reset must
require re-pairing friends. Uninstall or app-data loss loses the identity; no cloud
recovery or export is implemented. Credential-protected storage requires device
unlock after reboot.

Main APK packaging compiles this bridge with existing installer glue. Normal app
startup does not load it. [Standalone networking harness](android-sharing-probe.md)
uses this bridge with separate app identity and [Phase 2 pairing controls](sharing-pairing.md);
production sharing UI remains pending.

## Device tests

Build isolated instrumentation APK with installed SDK platform/build-tools 35:

```sh
export ANDROID_HOME=/path/to/android-sdk
bash android/tests/package.sh
adb install -r target/sharing-identity-test/identity-test.apk
adb shell am instrument -w dev.roamlog.sharingidentity.test/dev.roamlog.android.SharingIdentityTest
```

Expected: `Sharing identity checks passed`, instrumentation result code `-1`.
Test package has its own UID and Keystore namespace; its runner refuses any other
target package. Each run resets only disposable test identity. Production app
data and identities are not used.

Checks cover repeated loads, concurrent reads, non-exportable wrapping key,
tampered ciphertext, truncation, oversized records, unknown versions, AtomicFile
backup recovery, lost ciphertext, and lost wrapping keys. Device execution is
required: desktop JVM cannot validate Android Keystore or AtomicFile behavior.
Separate-process contention, reboot/restart behavior, hardware-backed storage,
and native adapter runtime validation still need device tests.
