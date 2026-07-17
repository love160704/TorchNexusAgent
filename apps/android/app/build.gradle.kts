import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

val localKeystorePropertiesFile = rootProject.file("keystore.properties")
val localKeystoreProperties = Properties().apply {
    if (localKeystorePropertiesFile.isFile) {
        localKeystorePropertiesFile.inputStream().use { load(it) }
    }
}

fun firstSigningValue(vararg values: String?): String? =
    values.firstOrNull { !it.isNullOrBlank() }

val releaseKeystorePath = firstSigningValue(
    providers.environmentVariable("ANDROID_KEYSTORE_FILE").orNull,
    providers.gradleProperty("android.injected.signing.store.file").orNull,
    localKeystoreProperties.getProperty("storeFile"),
)
val releaseKeystorePassword = firstSigningValue(
    providers.environmentVariable("ANDROID_KEYSTORE_PASSWORD").orNull,
    providers.gradleProperty("android.injected.signing.store.password").orNull,
    localKeystoreProperties.getProperty("storePassword"),
)
val releaseKeyAlias = firstSigningValue(
    providers.environmentVariable("ANDROID_KEY_ALIAS").orNull,
    providers.gradleProperty("android.injected.signing.key.alias").orNull,
    localKeystoreProperties.getProperty("keyAlias"),
)
val releaseKeyPassword = firstSigningValue(
    providers.environmentVariable("ANDROID_KEY_PASSWORD").orNull,
    providers.gradleProperty("android.injected.signing.key.password").orNull,
    localKeystoreProperties.getProperty("keyPassword"),
)

android {
    namespace = "com.torchnexus.agent"
    compileSdk = 35
    ndkVersion = "27.0.12077973"

    defaultConfig {
        applicationId = "com.torchnexus.agent"
        minSdk = 29
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"

        // The Rust libraries are built only for arm64-v8a.  Avoid packaging
        // compatibility libraries for device architectures we do not support.
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    signingConfigs {
        create("release") {
            storeFile = releaseKeystorePath?.let(rootProject::file)
            storePassword = releaseKeystorePassword
            keyAlias = releaseKeyAlias
            keyPassword = releaseKeyPassword
        }
    }

    buildTypes {
        getByName("release") {
            signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    packaging {
        jniLibs {
            excludes += setOf(
                "lib/armeabi/**",
                "lib/armeabi-v7a/**",
                "lib/mips/**",
                "lib/mips64/**",
                "lib/x86/**",
                "lib/x86_64/**",
                "**/libtun2proxy.so",
            )
        }
    }
}

val verifyReleaseSigning by tasks.registering {
    doLast {
        val missingSettings = listOf(
            "keystore file" to releaseKeystorePath,
            "keystore password" to releaseKeystorePassword,
            "key alias" to releaseKeyAlias,
            "key password" to releaseKeyPassword,
        ).filter { (_, value) -> value.isNullOrBlank() }
            .map { (name, _) -> name }

        check(missingSettings.isEmpty()) {
            "Release signing is not configured. Use Android Studio's Generate Signed Bundle or APK, " +
                "set the ANDROID_* signing environment variables, or create apps/android/keystore.properties. " +
                "Missing: ${missingSettings.joinToString()}"
        }

        val keystore = rootProject.file(checkNotNull(releaseKeystorePath))
        check(keystore.isFile) { "Release keystore was not found: $keystore" }
    }
}

tasks.configureEach {
    if (name == "packageRelease" || name == "assembleRelease" || name == "bundleRelease") {
        dependsOn(verifyReleaseSigning)
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.17.0@aar")
    implementation("androidx.security:security-crypto:1.1.0")
    implementation(platform("androidx.compose:compose-bom:2024.09.03"))
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.compose.material3:material3")
    testImplementation("junit:junit:4.13.2")
}
