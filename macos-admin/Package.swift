// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "RouterAdmin",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "RouterAdmin",
            path: "Sources/RouterAdmin"
        ),
    ]
)
