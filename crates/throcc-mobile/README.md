# throcc-mobile

The UniFFI layer between `throcc-client-core` and the iOS and Android apps in `mobile/`. It builds as a static library for iOS and a shared library for Android. The core has no binding code: everything the apps call is declared here.

| File | Holds |
| --- | --- |
| `src/lib.rs` | What the apps call: the directories the app keeps its data in, and our identity key |
| `src/bin/uniffi-bindgen.rs` | The bindings generator, behind the `bindgen` feature so the libraries don't carry it |
| `uniffi.toml` | The Kotlin package of the generated bindings |

The bindings are generated from a library built for the phone, not the host: some calls exist only there.

```sh
cargo run -p throcc-mobile --features bindgen --bin uniffi-bindgen -- generate --language swift --out-dir <dir> <library>
```

`mobile/ios/build-core.sh` and the Android Gradle build run this themselves.
