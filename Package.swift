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
            url: "https://github.com/project-minigraf/minigraf/releases/download/v1.1.1/MinigrafKit-v1.1.1.xcframework.zip",
            checksum: "ac3d4defc08b547ad5f6b60c4c5a542a53161558521d54c4af4b39e7e81554cd"
        ),
        .target(
            name: "MinigrafKit",
            dependencies: [.target(name: "minigrafFFI")],
            path: "minigraf-swift/Sources/MinigrafKit"
        ),
    ]
)
