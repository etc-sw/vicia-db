// swift-tools-version: 5.9
import PackageDescription

// This file is automatically updated by CI after each release.
// The URL and checksum below are updated to point to the latest .xcframework.zip.
let package = Package(
    name: "MinigrafKit",
    platforms: [
        .iOS(.v16),
    ],
    products: [
        .library(
            name: "MinigrafKit",
            targets: ["minigrafFFI", "MinigrafKit"]
        ),
    ],
    targets: [
        .binaryTarget(
            name: "minigrafFFI",
            // Updated by CI: release-upload-mobile job
            url: "https://github.com/project-minigraf/minigraf/releases/download/v1.1.0/MinigrafKit-v1.1.0.xcframework.zip",
            checksum: "770c19a0bb94ff2cd54228d83af9e7c4e10f3993921b94b179e625f9ee1d6f6f"
        ),
        .target(
            name: "MinigrafKit",
            dependencies: [.target(name: "minigrafFFI")],
            path: "minigraf-swift/Sources/MinigrafKit"
        ),
    ]
)
