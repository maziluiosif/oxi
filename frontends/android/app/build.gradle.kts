import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The app ships with oxi and carries its version: the one in the workspace's Cargo.toml.
// `-PoxiVersion=X.Y.Z` overrides it, and `-PoxiUpdateFeed=URL` points the updater at another
// feed: both only for testing updates without publishing a release.
val oxiVersion: String = (findProperty("oxiVersion") as String?) ?: rootDir.resolve("../../Cargo.toml").readLines()
    .first { it.startsWith("version") }
    .substringAfter('"').substringBefore('"')
val updateFeed: String = (findProperty("oxiUpdateFeed") as String?) ?: "https://github.com/maziluiosif/oxi/releases"
val oxiVersionCode: Int = oxiVersion.split('.').map { it.toInt() }
    .let { (major, minor, patch) -> major * 1_000_000 + minor * 1_000 + patch }

// Release signing: env vars in CI (see .github/workflows/release.yml), else
// ~/.config/oxi-android/keystore.properties on a developer machine. Every build that may
// update an installed app has to be signed with this same key.
val releaseKey: Map<String, String>? = run {
    System.getenv("OXI_ANDROID_KEYSTORE")?.takeIf { it.isNotBlank() }?.let { store ->
        return@run mapOf(
            "storeFile" to store,
            "storePassword" to System.getenv("OXI_ANDROID_KEYSTORE_PASSWORD"),
            "keyAlias" to (System.getenv("OXI_ANDROID_KEY_ALIAS") ?: "oxi"),
            "keyPassword" to System.getenv("OXI_ANDROID_KEYSTORE_PASSWORD"),
        )
    }
    val file = File(System.getProperty("user.home"), ".config/oxi-android/keystore.properties")
    if (!file.exists()) return@run null
    val props = Properties().apply { file.inputStream().use { load(it) } }
    props.stringPropertyNames().associateWith { props.getProperty(it) }
}

android {
    namespace = "dev.oxi.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.oxi.android"
        minSdk = 29
        targetSdk = 35
        versionCode = oxiVersionCode
        versionName = oxiVersion
        buildConfigField("String", "UPDATE_FEED", "\"$updateFeed\"")
    }

    signingConfigs {
        if (releaseKey != null) {
            create("release") {
                storeFile = file(releaseKey.getValue("storeFile"))
                storePassword = releaseKey.getValue("storePassword")
                keyAlias = releaseKey.getValue("keyAlias")
                keyPassword = releaseKey.getValue("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            // Without the release key (a fork, a fresh machine) fall back to the debug key: such
            // a build installs, but can't update an app signed with the release key.
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.06.01")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.1")
    implementation("androidx.lifecycle:lifecycle-process:2.9.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("androidx.core:core-ktx:1.16.0")
    // Play services brings an old Fragment, whose activity results (camera, permissions) are broken.
    implementation("androidx.fragment:fragment-ktx:1.8.8")
    // Google's QR scanner UI from Play services: no camera permission, nothing bundled.
    implementation("com.google.android.gms:play-services-code-scanner:16.1.0")
    debugImplementation("androidx.compose.ui:ui-tooling")
    testImplementation("junit:junit:4.13.2")
    // The real org.json: Android's is only a stub in unit tests.
    testImplementation("org.json:json:20250517")
}
