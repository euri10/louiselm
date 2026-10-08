plugins {
    id("com.android.application")
}

// A configured build still requires the user's in-app notification opt-in.
val firebaseChoice = providers.gradleProperty("louiselmFirebase").orElse("false").get()
require(firebaseChoice in setOf("true", "false")) { "louiselmFirebase must be true or false" }
val firebaseEnabled = firebaseChoice == "true"
if (firebaseEnabled) apply(plugin = "com.google.gms.google-services")

android {
    namespace = "dev.louiselm.capture"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.louiselm.capture"
        minSdk = 28
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
        buildConfigField("boolean", "FIREBASE_ENABLED", firebaseEnabled.toString())
    }

    buildFeatures { buildConfig = true }

    buildTypes {
        create("qa") {
            initWith(getByName("debug"))
            applicationIdSuffix = ".qa"
            versionNameSuffix = "-qa"
            matchingFallbacks += "debug"
        }
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
        // A newer upstream release is not a defect in this commit: these checks
        // turned unrelated commits red on Maven's release calendar
        // (louiselm-dy2mp, louiselm-g4uf3, louiselm-9gjnk). Dependabot proposes
        // upgrades as tested pull requests instead (.github/dependabot.yml).
        disable.addAll(listOf("AndroidGradlePluginVersion", "GradleDependency", "NewerVersionAvailable"))
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
    implementation("androidx.core:core:1.19.1")
    implementation("androidx.work:work-runtime:2.12.0")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
    implementation("com.google.firebase:firebase-messaging:26.0.0")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.17")
    // The Android framework's org.json stubs omit key iteration in local JVM tests.
    testImplementation("org.json:json:20260814")
}
