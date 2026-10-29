import Foundation
import Network
import Testing
import UIKit
import WebKit
@testable import KithUI

/// Diagrams draw with the bundled mermaid.js; anything mermaid can't draw is nil, so the chat shows the code.
@MainActor @Suite(.serialized) struct MermaidRendererTests {
    @Test func drawsAFlowchartAndASequenceDiagram() async throws {
        let renderer = MermaidRenderer()
        let flowchart = try #require(await renderer.image("graph TD\nA[Start] --> B{Ready?}\nB -->|yes| C[Ship]\nB -->|no| A", dark: false))
        #expect(flowchart.size.width > 50 && flowchart.size.height > 100)
        #expect(fillsItsPicture(flowchart))
        let sequence = try #require(await renderer.image("sequenceDiagram\nKai->>coder: add diagrams\ncoder-->>Kai: PR", dark: true))
        #expect(sequence.size.width > 100 && sequence.size.height > 50)
        #expect(fillsItsPicture(sequence))
        #expect(renderer.cached("sequenceDiagram\nKai->>coder: add diagrams\ncoder-->>Kai: PR", dark: true) === sequence)
    }

    @Test func aBrokenOrOversizedDiagramIsNotDrawn() async {
        let renderer = MermaidRenderer()
        #expect(await renderer.image("graph TD\nA -->", dark: false) == nil)
        #expect(await renderer.image("not a diagram", dark: false) == nil)
        #expect(await renderer.image("", dark: false) == nil)
        #expect(await renderer.image("graph TD\n" + String(repeating: "A-->B\n", count: 4_000), dark: false) == nil)
        // The page still draws after a failure.
        #expect(await renderer.image("graph LR\nA-->B", dark: false) != nil)
    }

    /// Links, click directives, HTML in labels and a diagram's own config that asks for less security:
    /// nothing reaches the network and none of the diagram's script runs.
    @Test func aHostileDiagramLoadsNothingAndRunsNothing() async throws {
        let server = try await Listener()
        let url = "http://127.0.0.1:\(server.port)"
        // The listener sees what reaches it.
        _ = try? await URLSession.shared.data(from: URL(string: "\(url)/control")!)
        #expect(server.requests.contains("GET /control HTTP/1.1"))

        let hostile = [
            "graph TD\nA[\"<img src='\(url)/img' onerror='window.pwned=1'>\"] --> B[\"<script>window.pwned=1</script>\"]",
            "graph TD\nA-->B\nclick A href \"\(url)/href\"\nclick B call pwn()\nclick A \"javascript:window.pwned=1\"",
            "%%{init: {\"securityLevel\": \"loose\", \"htmlLabels\": true, \"flowchart\": {\"htmlLabels\": true}}}%%\ngraph TD\nA[\"<img src='\(url)/init' onerror='window.pwned=1'>\"]\nclick A call pwn()",
            "---\nconfig:\n  securityLevel: loose\n  htmlLabels: true\n---\ngraph TD\nA[\"<b><img src='\(url)/front' onerror='window.pwned=1'></b>\"]",
            "%%{init: {\"themeCSS\": \"@import url(\(url)/css); .node{background:url(\(url)/bg)}\"}}%%\ngraph TD\nA-->B\nstyle A fill:url(\(url)/fill)",
            "sequenceDiagram\nA->>B: hi\nlink A: Docs @ \(url)/link\nlinks B: {\"Docs\": \"\(url)/links\"}",
        ]
        let renderer = MermaidRenderer()
        for source in hostile {
            _ = await renderer.image(source, dark: false)
        }
        let webView = try #require(renderer.webView)
        // Nor can a script on the page take it anywhere.
        _ = try? await webView.callAsyncJavaScript("location.href = url + '/nav'", arguments: ["url": url], contentWorld: .page)
        try await Task.sleep(for: .seconds(1))

        #expect(server.requests.allSatisfy { $0 == "GET /control HTTP/1.1" })
        let pwned = try await webView.callAsyncJavaScript("return typeof window.pwned", contentWorld: .page)
        #expect(pwned as? String == "undefined")
        #expect(await renderer.image("graph LR\nA-->B", dark: false) != nil)
    }
}

/// The drawing reaches near the far edges of the picture (mermaid leaves a margin): it isn't drawn shrunk into one corner.
@MainActor private func fillsItsPicture(_ image: UIImage) -> Bool {
    guard let cg = image.cgImage else { return false }
    let (width, height) = (cg.width, cg.height)
    var pixels = [UInt8](repeating: 0, count: width * height * 4)
    let drawn = pixels.withUnsafeMutableBytes { buffer in
        CGContext(data: buffer.baseAddress, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
                  space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            .map { $0.draw(cg, in: CGRect(x: 0, y: 0, width: width, height: height)); return true } ?? false
    }
    guard drawn else { return false }
    var (right, bottom) = (0, 0)
    for y in 0..<height {
        for x in 0..<width where pixels[(y * width + x) * 4 + 3] > 0 {
            right = max(right, x)
            bottom = max(bottom, y)
        }
    }
    return right > width * 3 / 4 && bottom > height * 3 / 4
}

/// A local server that notes each request's first line (or that a connection came) and answers none.
private final class Listener: @unchecked Sendable {
    private let listener: NWListener
    private let lock = NSLock()
    private var seen: [String] = []

    var requests: [String] { lock.withLock { seen } }
    var port: UInt16 { listener.port?.rawValue ?? 0 }

    init() async throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
        listener.newConnectionHandler = { [self] connection in
            connection.start(queue: .global())
            connection.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [self] data, _, _, _ in
                let line = data.flatMap { String(data: $0, encoding: .utf8) }?.components(separatedBy: "\r\n").first
                lock.withLock { seen.append(line ?? "a connection") }
                connection.cancel()
            }
        }
        listener.start(queue: .global())
        while port == 0 { try await Task.sleep(for: .milliseconds(10)) }
    }

    deinit { listener.cancel() }
}
