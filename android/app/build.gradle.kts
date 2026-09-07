plugins {
    id("com.android.application")
}

android {
    namespace = "dev.louiselm.capture"
    // Tested SDK/CI pair; upgrading to 37 is tracked in louiselm-myd5.
    //noinspection GradleDependency
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.louiselm.capture"
        minSdk = 28
        // Raising this opts into runtime changes; validate them in louiselm-myd5.
        //noinspection OldTargetApi
        targetSdk = 36
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

dependencies {
    // 1.19 requires the SDK upgrade tracked in louiselm-myd5.
    //noinspection GradleDependency
    implementation("androidx.core:core:1.17.0")
    implementation("androidx.work:work-runtime:2.11.2")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.16.1")
    // The Android framework's org.json stubs omit key iteration in local JVM tests.
    // Keep the tested parser fixture pinned until the louiselm-myd5 compatibility audit.
    //noinspection NewerVersionAvailable
    testImplementation("org.json:json:20240303")
}
