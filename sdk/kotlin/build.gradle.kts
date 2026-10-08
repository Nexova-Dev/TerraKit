import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// Whether an Android SDK is discoverable.
//
// ADR-008 explains why this is a decision at configuration time rather than a build failure:
// an unconditional `androidTarget()` makes the whole build unrunnable on a machine without the
// SDK, and the consequence is that the cross-SDK conformance suite stops running on Linux CI,
// which is the only place §14's comparison is actually enforced. A build that cannot run its own
// tests is worse than a build with one fewer target.
val androidSdkHome: String? by lazy {
    val localProperties = file("local.properties")
    if (localProperties.isFile) {
        val fromLocal = localProperties.readLines()
            .firstOrNull { it.trimStart().startsWith("sdk.dir=") }
            ?.substringAfter("=")
            ?.trim()
        if (fromLocal != null && file(fromLocal).isDirectory) return@lazy fromLocal
    }
    for (variable in listOf("ANDROID_HOME", "ANDROID_SDK_ROOT")) {
        val value = System.getenv(variable) ?: System.getProperty(variable) ?: continue
        if (file(value).isDirectory) return@lazy value
    }
    null
}

plugins {
    kotlin("multiplatform") version "2.2.10"
    // Applied below rather than declared as `apply false`, because it is genuinely conditional
    // and an unused AGP plugin on the classpath is not free: it registers tasks that then fail to
    // configure without a compileSdk.
    id("com.android.library") version "8.13.2" apply false
}

kotlin {
    // No `jvmToolchain`. Targeting 17 is done with `jvmTarget` alone, so the build runs on whatever
    // JDK it finds rather than demanding a provisioned JDK 17: toolchain auto-provisioning would
    // add a resolver plugin and a download to a library that needs neither, and the emitted
    // bytecode is identical either way. The SDK uses no JDK API newer than 17, so compiling against
    // a newer launcher and emitting 17 is sound.
    //
    // 17 rather than the launcher's version because Android desugaring and the JDK an app is built
    // with both bottom out around there, and a library compiled against 25 bytecode cannot be
    // consumed by an app that has not opted in.

    jvm {
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_17)
        }
    }

    if (androidSdkHome != null) {
        apply(plugin = "com.android.library")

        androidTarget {
            compilerOptions {
                jvmTarget.set(JvmTarget.JVM_17)
            }
        }
    } else {
        logger.lifecycle(
            "terrakit: no Android SDK found (ANDROID_HOME, ANDROID_SDK_ROOT, sdk.dir). " +
                "Building the JVM target only; `assembleAndroid` is not configured. See ADR-008."
        )
    }

    // An intermediate source set shared by the JVM and Android compilations.
    //
    // It exists because normalization needs `String.codePoints()` and `java.text.Normalizer`, which
    // Kotlin's common stdlib does not have — both targets are on a JVM, but `commonMain` cannot see
    // `java.lang`. Declaring the hierarchy explicitly rather than relying on the default template,
    // because the template only creates intermediate sets for a fixed list of target families.
    //
    // Everything in here is therefore JVM-and-Android only, which is the entire platform list:
    // ADR-007 defers Kotlin/Native, so there is no target that would be excluded.
    //
    // The split is not "pure Kotlin" versus "platform code". `commonMain` keeps the value types and
    // the `SqlHandle` contract -- models, errors, the row interface, the version floor -- and
    // everything that needs to name a file lives here: the resolver, the facade, and `Queries`. The
    // query engine is in this set because it takes a `Package`, and a `Package` owns a `File`; the
    // alternative was threading an opaque package handle through every signature to keep the SQL out
    // of the shared set, which buys nothing for a project with no native target.
    applyDefaultHierarchyTemplate()
    sourceSets {
        val jvmSharedMain by creating {
            dependsOn(commonMain.get())
        }
        // Created explicitly rather than with `by getting`: Kotlin derives the *main* name from the
        // source set but not a matching test one, so the test source set has to be declared. The
        // dependencies below mirror the main hierarchy exactly, because a shared main with no shared
        // test is how the JVM-only fixture suite ends up silently absent from Android.
        val jvmSharedTest by creating {
            dependsOn(commonTest.get())
        }
        jvmMain.get().dependsOn(jvmSharedMain)
        jvmTest.get().dependsOn(jvmSharedTest)
        if (androidSdkHome != null) {
            androidMain.get().dependsOn(jvmSharedMain)
            androidUnitTest.get().dependsOn(jvmSharedTest)
        }
    }

    sourceSets {
        commonMain.dependencies {
            // No dependency at all. Everything the SDK does that is not statement execution is
            // pure Kotlin over strings and values.
        }

        commonTest.dependencies {
            implementation(kotlin("test"))
        }

        getByName("jvmSharedTest").dependencies {
            implementation(kotlin("test"))
        }

        jvmMain.dependencies {
            // The one artifact ADR-008 allows. It is the JVM's binding to the SQLite C library —
            // the same position libsqlite3 occupies in the Swift SDK — and it brings no query
            // builder, no ORM and no serialization.
            implementation("org.xerial:sqlite-jdbc:3.41.2.2")
        }

        jvmTest.dependencies {
            implementation(kotlin("test-junit"))
        }

        if (androidSdkHome != null) {
            // Nothing. `android.database.sqlite` is in the platform.
            androidMain.dependencies {
            }
        }
    }
}

if (androidSdkHome != null) {
    extensions.configure<com.android.build.gradle.LibraryExtension>("android") {
        namespace = "com.terrakit.core"
        compileSdk = 34

        defaultConfig {
            // 21 rather than a recent release: the SDK uses nothing newer, and a lower floor is
            // a wider set of devices for a library that is only a database reader.
            minSdk = 21
        }

        compileOptions {
            sourceCompatibility = JavaVersion.VERSION_17
            targetCompatibility = JavaVersion.VERSION_17
        }
    }
}

// Task wiring for the conformance suite.
//
// `crossSdkConformance` is named as a separate task so CI can ask for exactly that, rather than
// running every test in the module and hoping the conformance ones were included. The env var is
// read here because a test that silently skips is worse than a test that fails: `checkConformance`
// fails when the packages directory is named but unreadable, which is a different thing from it
// being unset.
val conformancePackages: String? = System.getenv("TERRAKIT_CONFORMANCE_PACKAGES")

tasks.register("crossSdkConformance") {
    group = "verification"
    description = "Run the shared conformance battery against built packages"

    val packagesDir = conformancePackages
    doFirst {
        if (packagesDir == null) {
            throw GradleException(
                """
                TERRAKIT_CONFORMANCE_PACKAGES is not set.

                The conformance suite needs built packages to compare against. It is not run
                by `check`, because a suite that quietly skips when its fixtures are absent
                reports coverage it does not have.

                  python3 tools/conformance.py generate --packages <datasets>
                  TERRAKIT_CONFORMANCE_PACKAGES=<datasets> ./gradlew crossSdkConformance
                """.trimIndent()
            )
        }
        if (!file(packagesDir).isDirectory) {
            throw GradleException("TERRAKIT_CONFORMANCE_PACKAGES=$packagesDir is not a directory")
        }
    }

    dependsOn("jvmTest")
}

// Point the tests at the repository fixtures.
//
// `TERRAKIT_REPO_ROOT` is set **unconditionally**, not alongside the package directory. The
// normalization suite reads the golden vectors from the repository and needs no packages, so
// tying it to the package directory made it fail on a checkout that had packages built but not
// exported — which is the shape of a contributor's machine.
tasks.withType<Test>().configureEach {
    environment("TERRAKIT_REPO_ROOT", rootDir.parentFile.parentFile.absolutePath)
    if (conformancePackages != null) {
        environment("TERRAKIT_CONFORMANCE_PACKAGES", file(conformancePackages).absolutePath)
    }
}