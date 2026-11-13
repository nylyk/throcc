import java.io.ByteArrayOutputStream
import java.io.OutputStream
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
        val codecs = codecs()
        val targets = abis.get().flatMap { listOf("-t", it) }
        cargo(
            codecs, "ndk", *targets.toTypedArray(), "-P", "31", "-o", libraries.get().asFile.path,
            "build", "--locked", "-p", "throcc-mobile", "--release",
        )
        cargo(
            codecs, "run", "--locked", "-p", "throcc-mobile", "--features", "bindgen", "--bin", "uniffi-bindgen", "--",
            "generate", "--language", "kotlin", "--no-format", "--out-dir", bindings.get().asFile.path,
            libraries.file("${abis.get().first()}/libthrocc_mobile.so").get().asFile.path,
        )
    }

    /** The Rust target each ABI builds for. */
    private val rustTargets = mapOf("arm64-v8a" to "aarch64-linux-android", "x86_64" to "x86_64-linux-android")

    /** Builds dav1d and libvpx for any ABI that lacks them, and returns how cargo links them. */
    private fun codecs(): Map<String, String> {
        val root = repository.get().dir("deps/codecs").asFile
        // uniffi-bindgen builds the core for the host too, which links them on a Mac.
        val printedHost = ByteArrayOutputStream()
        run(emptyMap(), "rustc", "-vV", output = printedHost)
        val host = printedHost.toString().lines().first { it.startsWith("host: ") }.removePrefix("host: ")
        val targets = abis.get().map { rustTargets.getValue(it) } + listOf(host).filter { "apple" in it }
        val missing = targets.filter { !root.resolve("$it/lib/libvpx.a").exists() }
        if (missing.isNotEmpty()) run(emptyMap(), "packaging/codecs/build.sh", root.path, *missing.toTypedArray())
        val printed = ByteArrayOutputStream()
        run(emptyMap(), "packaging/codecs/env.sh", root.path, output = printed)
        return printed.toString().lines().filter { "=" in it }.associate { it.substringBefore("=") to it.substringAfter("=") }
    }

    private fun cargo(environment: Map<String, String>, vararg arguments: String) = run(environment, "cargo", *arguments)

    private fun run(environment: Map<String, String>, vararg command: String, output: OutputStream? = null) {
        exec.exec {
            workingDir = repository.get().asFile
            environment("ANDROID_NDK_HOME", ndk.get().asFile.path)
            environment("PATH", "${System.getProperty("user.home")}/.cargo/bin${File.pathSeparator}${System.getenv("PATH")}")
            environment(environment)
            if (output != null) standardOutput = output
            commandLine(*command)
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
