# throcc-mobile

The UniFFI layer between `throcc-client-core` and the iOS and Android apps in `mobile/`. It builds as a static library for iOS and a shared library for Android. The core has no binding code: everything the apps call is declared here. Media never crosses it: a state is plain data, and video and audio stay in Rust, down to the audio devices and the views shares are shown in. An app hands a share's view over with `Core::show_share` as a pointer it gives up its reference to, and takes it back with `Core::hide_share`: an Android `Surface`'s `ANativeWindow`, from `ShareView.nativeWindow`, this crate's one JNI function, or an iOS `AVSampleBufferDisplayLayer`.

| File | Holds |
| --- | --- |
| `src/lib.rs` | `Core`, the one object an app holds: what the install keeps (our profile, the theme, known servers, verified and blocked people) and the connection to one server at a time, over `Client` or, for `Core::mock`, `MockClient`. An app registers a `Listener`, which hears every state and event in order on one thread; what a connection still says after it ended is dropped, and events a slow listener falls behind on are skipped. Connecting, disconnecting and forgetting a server run one at a time and block until done. A real connection owns the audio engine: the app sets up the platform's audio session, then opens the speaker and the mic with `open_audio` for as long as it is in a room. Also the name, password, picture and invite checks with their wording, the theme colours, the volume curve, what each role may do, structured sign-in errors (a changed server key, a missing identity) with `trust_server_key` and `start_over`, and each share's views: the newest one shown shows it, the one before takes over when it goes |
| `src/state.rs` | `CoreState` (the core's `State`, named apart from SwiftUI's and Compose's own `State`) and its parts as UniFFI records and enums, built from the core's with `From` impls. Ids are `u64`, keys, fingerprints and picture hashes are bytes |
| `src/safety.rs` | `SafetyCheck`, the core's safety-number comparison as an object a profile holds: it follows the key presented on every state and sends only the key whose number is on screen |
| `src/messages.rs` | `Command`, turned into the core's `UiCommand`, and `Event`, built from its `UiEvent` |
| `src/voice.rs` | `Voice`, the voice settings a phone offers (voice activation sensitivity, speech detection, noise suppression, automatic gain and the two volumes), and `MicLevel` for the level meter. `Core::set_voice` applies them to the call as a slider moves and keeps them once let go; a phone always sends by voice activation |
| `src/bin/uniffi-bindgen.rs` | The bindings generator, behind the `bindgen` feature so the libraries don't carry it |
| `uniffi.toml` | The Kotlin package of the generated bindings and the frameworks the Swift module links |

The bindings are generated from a library built for the phone, not the host: some calls exist only there.

```sh
cargo run -p throcc-mobile --features bindgen --bin uniffi-bindgen -- generate --language swift --out-dir <dir> <library>
```

`mobile/ios/build-core.sh` and the Android Gradle build run this themselves.
