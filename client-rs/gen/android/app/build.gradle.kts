import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Kunci tanda tangan TETAP CloudPC (bukan kunci debug per-laptop): tanpa kunci yang sama, Android menolak memasang
// pembaruan di atas versi terpasang. Lokasi berkas properti diberikan build-android.ps1 lewat variabel lingkungan; berkas
// kunci dan kata sandinya TIDAK ada di repo. Tanpa itu build memakai kunci debug laptop (hanya untuk uji sendiri).
val cloudpcKeystore = Properties().apply {
    val path = System.getenv("CLOUDPC_ANDROID_KEYSTORE_PROPS")
    if (!path.isNullOrBlank() && file(path).exists()) {
        file(path).inputStream().use { load(it) }
    }
}
val cloudpcSigned = cloudpcKeystore.getProperty("storeFile") != null

android {
    compileSdk = 37
    signingConfigs {
        if (cloudpcSigned) {
            create("cloudpc") {
                storeFile = file(cloudpcKeystore.getProperty("storeFile"))
                storePassword = cloudpcKeystore.getProperty("storePassword")
                keyAlias = cloudpcKeystore.getProperty("keyAlias")
                keyPassword = cloudpcKeystore.getProperty("keyPassword")
            }
        }
    }
    namespace = "id.cloudpc.app"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "id.cloudpc.app"
        // 26 (Android 8.0): suara layar PC memakai AAudio, mouse fisik memakai pointer capture.
        minSdk = 26
        targetSdk = 37
        // Nomor versi harus NAIK tiap rilis supaya Android mau memasang pembaruan; build-android.ps1 mengisinya dari
        // tanggal + menit build. Tanpa variabel itu (build manual) dipakai nilai Tauri.
        versionCode = System.getenv("CLOUDPC_ANDROID_VERSION_CODE")?.toIntOrNull()
            ?: tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = System.getenv("CLOUDPC_RS_VERSION")?.takeIf { it.isNotBlank() }
            ?: tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    buildTypes {
        getByName("debug") {
            if (cloudpcSigned) signingConfig = signingConfigs.getByName("cloudpc")
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                // Pustaka dikompres di dalam APK (45 MB -> ±20 MB) supaya APK percobaan mudah dikirim ke HP.
                jniLibs.useLegacyPackaging = true
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
    // Layar PC (id.cloudpc.stream): sumber Kotlin-nya milik stream-rs (GPL-3.0) dan dikompilasi dari sana, tidak disalin.
    // Pustaka Rust-nya (libcloudpc_stream.so) ditaruh build-android.ps1 di jniLibs.
    sourceSets.getByName("main") {
        java.srcDir("../../../../stream-rs/android/kotlin")
        kotlin.srcDir("../../../../stream-rs/android/kotlin")
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
