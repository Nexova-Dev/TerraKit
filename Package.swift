// swift-tools-version:6.0

import PackageDescription

// TerraKit's Swift SDK.
//
// Zero third-party dependencies beyond `libsqlite3` and the standard library
// (SDK_SPEC.md §1.6). Foundation is used for Unicode normalization only -- String's own
// `decomposedStringWithCanonicalMapping` is what makes NFKD available without a package, and
// a dependency here would undercut the whole point of the offline SDK.
let package = Package(
    name: "TerraKit",
    platforms: [
        .macOS(.v13),
        .iOS(.v16),
    ],
    products: [
        .library(name: "TerraKit", targets: ["TerraKit"]),
    ],
    targets: [
        .target(
            name: "TerraKit",
            path: "sdk/swift/Sources/TerraKit",
            linkerSettings: [
                // libsqlite3 ships with the platform on Apple targets, so there is nothing to
                // vendor and nothing to build.
                .linkedLibrary("sqlite3"),
            ]
        ),
        .testTarget(
            name: "TerraKitTests",
            dependencies: ["TerraKit"],
            path: "sdk/swift/Tests/TerraKitTests"
        ),
    ]
)
