ios_project := "mobile/ios/throcc.xcodeproj"
ios_build := "mobile/ios/build"
ios_app := "cc.thro.throcc"

[private]
default:
    @just --list

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
    xcrun simctl launch --terminate-running-process "{{ device }}" {{ ios_app }} {{ args }}

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
    xcrun devicectl device process launch --terminate-existing --device "{{ device }}" {{ ios_app }} {{ args }}
