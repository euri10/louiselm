plugins {
    id("com.android.application")
}

android {
    namespace = "dev.louiselm.capture"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.louiselm.capture"
        minSdk = 28
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"

    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    testOptions {
        unitTests.isIncludeAndroidResources = true
    }

    lint {
        abortOnError = true
        warningsAsErrors = true
    }
}

kotlin {
    compilerOptions {
        allWarningsAsErrors = true
    }
}

tasks.withType<Test>().configureEach {
    // Robolectric's API37 ApplicationSharedMemory setup accesses SharedSecrets.
    jvmArgs("--add-exports=java.base/jdk.internal.access=ALL-UNNAMED")
}

dependencies {
    implementation("androidx.core:core:1.19.0")
    implementation("androidx.work:work-runtime:2.11.2")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")

    testImplementation("junit:junit:4.13.2")
    // Android 17 framework coverage first appears in the 4.17 prereleases.
    testImplementation("org.robolectric:robolectric:4.17-beta-4")
    // The Android framework's org.json stubs omit key iteration in local JVM tests.
    testImplementation("org.json:json:20260814")
}
