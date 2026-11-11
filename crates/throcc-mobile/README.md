# throcc-mobile

The UniFFI layer between `throcc-client-core` and the iOS and Android apps in `mobile/`. It builds as a static library for iOS and a shared library for Android. The core has no binding code: everything the apps call is declared here. Media never crosses it: a state is plain data, and video and audio stay in Rust.

| File | Holds |
| --- | --- |
| `src/lib.rs` | `Core`, the one object an app holds: what the install keeps (our profile, the theme, known servers, verified and blocked people) and the connection to one server at a time, over `Client` or, for `Core::mock`, `MockClient`. An app registers a `Listener`; a thread per connection passes every state and event on to it. Also the name, password and invite checks, and what each role may do |
| `src/state.rs` | `CoreState` (the core's `State`, named apart from SwiftUI's and Compose's own `State`) and its parts as UniFFI records and enums, built from the core's with `From` impls. Ids are `u64`, keys, fingerprints and picture hashes are bytes |
| `src/messages.rs` | `Command`, turned into the core's `UiCommand`, and `Event`, built from its `UiEvent` |
| `src/bin/uniffi-bindgen.rs` | The bindings generator, behind the `bindgen` feature so the libraries don't carry it |
| `uniffi.toml` | The Kotlin package of the generated bindings |

The bindings are generated from a library built for the phone, not the host: some calls exist only there.

```sh
cargo run -p throcc-mobile --features bindgen --bin uniffi-bindgen -- generate --language swift --out-dir <dir> <library>
```

`mobile/ios/build-core.sh` and the Android Gradle build run this themselves.
