plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// ONNX Runtime used for face detection. The AAR ships libonnxruntime.so for
// every ABI; the Rust library loads it at runtime (`ort-dynamic`). Keep it in
// sync with ORT_VERSION in mobile/scripts/build_ios.sh.
val onnxRuntimeVersion = "1.30.0"

// ABIs the Rust library is built for (x86 is not: no 32-bit emulators needed).
val rustAbis = listOf("arm64-v8a", "armeabi-v7a", "x86_64")
val rustJniLibs = layout.buildDirectory.dir("rustJniLibs")

android {
    namespace = "org.mediaanonymizer"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        applicationId = "org.mediaanonymizer"
        minSdk = maxOf(flutter.minSdkVersion, 24)
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
        ndk {
            abiFilters += rustAbis
        }
    }

    sourceSets {
        getByName("main") {
            // A plain directory: AGP 9 refuses Providers in the SourceSet API.
            jniLibs.srcDir(rustJniLibs.get().asFile)
        }
    }

    // Release signing from the environment (used by the release workflow):
    // MEDIAANON_KEYSTORE (path), MEDIAANON_KEYSTORE_PASSWORD, MEDIAANON_KEY_ALIAS,
    // MEDIAANON_KEY_PASSWORD. Without them release builds use the debug key, so
    // `flutter run --release` works out of the box.
    val keystore = System.getenv("MEDIAANON_KEYSTORE")?.takeIf { it.isNotEmpty() }
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("MEDIAANON_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("MEDIAANON_KEY_ALIAS")
                keyPassword = System.getenv("MEDIAANON_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.getByName(if (keystore != null) "release" else "debug")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}

dependencies {
    implementation("com.microsoft.onnxruntime:onnxruntime-android:$onnxRuntimeVersion")
}

// Builds crates/mobile for every ABI with cargo-ndk (`cargo install cargo-ndk`)
// into build/rustJniLibs/<abi>/libmediaanon_mobile.so. Always in release mode:
// the pixel kernels are far too slow unoptimised. Pass -Pmediaanon.skipRust=true
// to package libraries placed there by other means.
val buildRust by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds the MediaAnonymizer engine (crates/mobile) for Android."
    val repoRoot = rootProject.projectDir.resolve("../..").canonicalFile
    val minSdk = android.defaultConfig.minSdk!!
    val outDir = rustJniLibs.get().asFile
    enabled = !providers.gradleProperty("mediaanon.skipRust").map(String::toBoolean).getOrElse(false)
    workingDir = repoRoot
    doFirst {
        // Resolved at execution so that skipping this task does not require the NDK.
        val ndkDir = android.ndkDirectory.absolutePath
        environment("ANDROID_NDK_HOME", ndkDir)
        environment("ANDROID_NDK_ROOT", ndkDir)
    }
    commandLine(
        buildList {
            add("cargo")
            add("ndk")
            rustAbis.forEach { add("-t"); add(it) }
            addAll(listOf("--platform", minSdk.toString(), "-o", outDir.absolutePath))
            addAll(listOf("build", "--release", "--locked", "-p", "mediaanon-mobile"))
            addAll(listOf("--no-default-features", "--features", "ort-dynamic"))
        },
    )
}

tasks.named("preBuild") {
    dependsOn(buildRust)
}
