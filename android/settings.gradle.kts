pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

// Auto-provision the JDK the toolchain spec in app/build.gradle.kts asks for,
// so the build only needs *a* Java to launch Gradle with, not a specific JDK.
plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "tapview-android"

include(":app")
