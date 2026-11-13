app_id := "cc.thro.throcc"
ios_project := "mobile/ios/throcc.xcodeproj"
ios_build := "mobile/ios/build"
android_sdk := env("ANDROID_HOME", home_directory() / "Library/Android/sdk")
adb := android_sdk / "platform-tools/adb"
java_home := env("JAVA_HOME", "/Applications/Android Studio.app/Contents/jbr/Contents/Home")
server_data := "target/throcc-data"
sim := "iPhone 17"

[private]
default:
    @just --list

# Runs the desktop app in release, with any app arguments such as `--mock --screen rooms`.
desktop *args:
    #!/usr/bin/env bash
    set -euo pipefail
    host=$(rustc -vV | sed -n 's/^host: //p')
    [ -f "deps/codecs/$host/lib/libvpx.a" ] || packaging/codecs/build.sh deps/codecs "$host"
    set -a; eval "$(packaging/codecs/env.sh deps/codecs)"; set +a
    cargo run --release -p throcc-client -- {{ args }}

# Runs the server in release, keeping its key and database in `server_data`, with any server options such as `--listen [::]:8477`.
server *args:
    cargo run --release -p throcc-server -- --data "{{ server_data }}" {{ args }}

# Prints a single-use admin invite for the server keeping its data in `server_data`.
server-invite:
    cargo run --release -p throcc-server -- admin-invite --data "{{ server_data }}"

# Builds the Rust core and its Swift bindings into ThroccCore.xcframework, for `device`, `simulator` or both.
ios-core *slices:
    mobile/ios/build-core.sh {{ slices }}

# Builds the iOS app for the simulator.
ios-build-sim: (ios-core "simulator")
    xcodebuild -quiet -project {{ ios_project }} -scheme throcc -configuration Debug \
        -destination 'generic/platform=iOS Simulator' -derivedDataPath {{ ios_build }} build

# Runs the iOS app in the simulator `sim`, with any app arguments such as `--mock --screen rooms`.
ios-sim *args: ios-build-sim
    xcrun simctl bootstatus "{{ sim }}" -b
    -open -b com.apple.dt.Devices
    xcrun simctl install "{{ sim }}" {{ ios_build }}/Build/Products/Debug-iphonesimulator/throcc.app
    xcrun simctl launch --terminate-running-process "{{ sim }}" {{ app_id }} {{ args }}

# Builds the iOS app for a connected iPhone or iPad by name, registering it with the team.
ios-build-device device: (ios-core "device")
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

# Starts an Android emulator in the background and waits until it has booted.
android-emulator avd="Pixel_10_Pro":
    "{{ android_sdk }}/emulator/emulator" -list-avds | grep -qx "{{ avd }}" || { echo "no AVD named {{ avd }}" >&2; exit 1; }
    # The emulator hangs with host audio once the app opens the mic.
    nohup "{{ android_sdk }}/emulator/emulator" -avd "{{ avd }}" -no-audio >/dev/null 2>&1 &
    "{{ adb }}" -e wait-for-device
    until [ "$("{{ adb }}" -e shell getprop sys.boot_completed | tr -d '\r')" = 1 ]; do sleep 2; done

# Builds and installs the Android app with its Rust core, then starts it with any intent extras such as `--ez mock true --es screen rooms`.
android *args:
    ANDROID_HOME="{{ android_sdk }}" JAVA_HOME="{{ java_home }}" mobile/android/gradlew -p mobile/android installDebug \
        -Pandroid.injected.build.abi="$("{{ adb }}" shell getprop ro.product.cpu.abilist | tr -d '\r')"
    "{{ adb }}" shell am start -S -n {{ app_id }}/.MainActivity {{ args }}
