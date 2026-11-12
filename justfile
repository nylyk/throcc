app_id := "cc.thro.throcc"
ios_project := "mobile/ios/throcc.xcodeproj"
ios_build := "mobile/ios/build"
android_sdk := env("ANDROID_HOME", home_directory() / "Library/Android/sdk")
adb := android_sdk / "platform-tools/adb"
ffmpeg := "deps/ffmpeg-lgpl"
server_data := "target/throcc-data"

export ANDROID_HOME := android_sdk
export JAVA_HOME := env("JAVA_HOME", "/Applications/Android Studio.app/Contents/jbr/Contents/Home")

[private]
default:
    @just --list

# Runs the desktop app in release, with any app arguments such as `--mock --screen rooms`.
desktop *args:
    [ -f {{ ffmpeg }}/lib/pkgconfig/libavcodec.pc ] || packaging/ffmpeg/build.sh {{ ffmpeg }}
    FFMPEG_PKG_CONFIG_PATH="$PWD/{{ ffmpeg }}/lib/pkgconfig" cargo run --release -p throcc-client --features ffmpeg -- {{ args }}

# Runs the server in release on port 8476, keeping its key and database in `data`.
server data=server_data *args:
    cargo run --release -p throcc-server -- --data {{ data }} {{ args }}

# Prints a single-use admin invite for the server keeping its data in `data`.
server-invite data=server_data:
    cargo run --release -p throcc-server -- admin-invite --data {{ data }}

# Builds the Rust core and its Swift bindings into ThroccCore.xcframework.
ios-core:
    mobile/ios/build-core.sh

# Builds the iOS app for the simulator.
ios-build-sim: ios-core
    xcodebuild -quiet -project {{ ios_project }} -scheme throcc -configuration Debug \
        -destination 'generic/platform=iOS Simulator' -derivedDataPath {{ ios_build }} build

# Runs the iOS app in a simulator, passing on app arguments such as `--mock --screen rooms`.
ios-sim device="iPhone 17" *args: ios-build-sim
    xcrun simctl boot "{{ device }}" 2>/dev/null || true
    open -b com.apple.dt.Devices
    xcrun simctl install "{{ device }}" {{ ios_build }}/Build/Products/Debug-iphonesimulator/throcc.app
    xcrun simctl launch --terminate-running-process "{{ device }}" {{ app_id }} {{ args }}

# Builds the iOS app for a connected iPhone or iPad by name, registering it with the team.
ios-build-device device: ios-core
    xcodebuild -quiet -project {{ ios_project }} -scheme throcc -configuration Debug \
        -destination "platform=iOS,name={{ device }}" -derivedDataPath {{ ios_build }} \
        -allowProvisioningUpdates -allowProvisioningDeviceRegistration build

# Lists the iPhones and iPads `ios-device` can install to.
ios-devices:
    xcrun devicectl list devices

# Installs and starts the iOS app on a connected iPhone or iPad by name, with any app arguments.
ios-device device *args: (ios-build-device device)
    xcrun devicectl device install app --device "{{ device }}" {{ ios_build }}/Build/Products/Debug-iphoneos/throcc.app
    xcrun devicectl device process launch --terminate-existing --device "{{ device }}" {{ app_id }} {{ args }}

# Builds the Android app and its Rust core into a debug APK.
android-build:
    mobile/android/gradlew -p mobile/android assembleDebug

# Starts an Android emulator in the background and waits until it has booted.
android-emulator avd="Pixel_10_Pro":
    # The emulator hangs with host audio once the app opens the mic.
    nohup "{{ android_sdk }}/emulator/emulator" -avd {{ avd }} -no-audio >/dev/null 2>&1 &
    "{{ adb }}" wait-for-device
    until [ "$("{{ adb }}" shell getprop sys.boot_completed | tr -d '\r')" = 1 ]; do sleep 2; done

# Installs and starts the Android app on the one connected phone or emulator, with any intent extras such as `--ez mock true --es screen rooms`.
android *args: android-build
    "{{ adb }}" install -r mobile/android/app/build/outputs/apk/debug/app-debug.apk
    "{{ adb }}" shell am start -S -n {{ app_id }}/.MainActivity {{ args }}
