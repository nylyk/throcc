import javax.inject.Inject

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

val abis = listOf("arm64-v8a", "x86_64")

android {
    namespace = "cc.thro.throcc"
    compileSdk = 37
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "cc.thro.throcc"
        minSdk = 31
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += abis
        }
    }

    buildFeatures {
        compose = true
    }

    val keystore = providers.environmentVariable("ANDROID_KEYSTORE_FILE")
    if (keystore.isPresent) {
        val password = providers.environmentVariable("ANDROID_KEYSTORE_PASSWORD").get()
        buildTypes.getByName("release").signingConfig = signingConfigs.create("release") {
            storeFile = file(keystore.get())
            storePassword = password
            keyAlias = "throcc"
            keyPassword = password
        }
    }
}

dependencies {
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.material3)
    implementation(libs.compose.material3.adaptive)
    implementation(libs.compose.material.icons)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.navigation3.runtime)
    implementation(libs.navigation3.ui)
    implementation(libs.activity.compose)
    implementation(libs.jna) {
        artifact { type = "aar" }
    }
}

/** Builds throcc-mobile for `abis` and generates its Kotlin bindings. */
abstract class BuildCore @Inject constructor(private val exec: ExecOperations) : DefaultTask() {
    @get:Input
    abstract val abis: ListProperty<String>

    @get:Internal
    abstract val repository: DirectoryProperty

    @get:Internal
    abstract val ndk: DirectoryProperty

    @get:OutputDirectory
    abstract val libraries: DirectoryProperty

    @get:OutputDirectory
    abstract val bindings: DirectoryProperty

    @TaskAction
    fun build() {
        val targets = abis.get().flatMap { listOf("-t", it) }
        cargo(
            "ndk", *targets.toTypedArray(), "-P", "31", "-o", libraries.get().asFile.path,
            "build", "--locked", "-p", "throcc-mobile", "--release",
        )
        cargo(
            "run", "--locked", "-p", "throcc-mobile", "--features", "bindgen", "--bin", "uniffi-bindgen", "--",
            "generate", "--language", "kotlin", "--no-format", "--out-dir", bindings.get().asFile.path,
            libraries.file("${abis.get().first()}/libthrocc_mobile.so").get().asFile.path,
        )
    }

    private fun cargo(vararg arguments: String) {
        exec.exec {
            workingDir = repository.get().asFile
            environment("ANDROID_NDK_HOME", ndk.get().asFile.path)
            environment("PATH", "${System.getProperty("user.home")}/.cargo/bin${File.pathSeparator}${System.getenv("PATH")}")
            commandLine("cargo", *arguments)
        }
    }
}

/** The ABI an install from Android Studio or `just android` targets, else every ABI. */
val coreAbis = providers.gradleProperty("android.injected.build.abi")
    .map { device -> device.split(",").filter { it in abis }.take(1).ifEmpty { abis } }
    .orElse(abis)

androidComponents.onVariants { variant ->
    val buildCore = tasks.register<BuildCore>("build${variant.name.replaceFirstChar(Char::uppercase)}Core") {
        abis = coreAbis
        repository = rootDir.resolve("../..")
        ndk = androidComponents.sdkComponents.ndkDirectory
        outputs.upToDateWhen { false }
    }
    variant.sources.jniLibs?.addGeneratedSourceDirectory(buildCore, BuildCore::libraries)
    variant.sources.kotlin?.addGeneratedSourceDirectory(buildCore, BuildCore::bindings)
}
