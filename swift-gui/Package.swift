// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "CoworkMergeApp",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(
            name: "CoworkMergeApp",
            targets: ["CoworkMergeApp"]
        ),
    ],
    targets: [
        .target(
            name: "CoworkMergeCore"
        ),
        .executableTarget(
            name: "CoworkMergeApp",
            dependencies: ["CoworkMergeCore"]
        ),
        .testTarget(
            name: "CoworkMergeCoreTests",
            dependencies: ["CoworkMergeCore"]
        ),
    ]
)
