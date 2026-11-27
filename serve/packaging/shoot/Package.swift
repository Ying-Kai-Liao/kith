// swift-tools-version: 5.9
import PackageDescription

// Takes the picture of a shared page for the Feed (macOS only): `kith-shoot <html> <out.png>`.
let package = Package(
    name: "kith-shoot",
    platforms: [.macOS(.v14)],
    targets: [.executableTarget(name: "kith-shoot", path: "Sources")]
)
