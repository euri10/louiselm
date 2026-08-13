plugins {
    id("com.android.application")
}

android {
    namespace = "dev.louiselm.capture"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.louiselm.capture"
        minSdk = 28
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
}

dependencies {
    implementation("androidx.core:core:1.17.0")
    implementation("androidx.work:work-runtime:2.11.2")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")

    testImplementation("junit:junit:4.13.2")
}
