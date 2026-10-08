// Gradle settings for the Kotlin SDK.
//
// `sdk/kotlin` is a single multiplatform module rather than a `core` + `jvm` + `android` split.
// The split was considered and rejected: the two platforms differ only in how a statement is
// executed, and a separate core module would have meant either duplicating every entity or
// depending on a module for every type. One `expect fun` for the connection is a smaller
// mechanism than the module graph it replaces.

pluginManagement {
    repositories {
        gradlePluginPortal()
        google()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "terrakit-kotlin"