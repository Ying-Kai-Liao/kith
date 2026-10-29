import KithKit
import SwiftUI
import UIKit

/// A ```mermaid fence drawn as its diagram, or its code block until it's drawn and whenever it can't be.
/// Wide diagrams scroll inside the block; a tap opens it full screen.
struct MermaidBlock: View {
    let source: String
    @Environment(\.colorScheme) private var colorScheme
    @State private var drawn: (source: String, dark: Bool, image: UIImage)?
    @State private var viewing = false

    var body: some View {
        let dark = colorScheme == .dark
        let image = drawn.flatMap { $0.source == source && $0.dark == dark ? $0.image : nil }
            ?? MermaidRenderer.shared.cached(source, dark: dark)
        Group {
            if let image {
                ScrollView(.horizontal, showsIndicators: false) {
                    Button { viewing = true } label: {
                        Image(uiImage: image)
                            .resizable()
                            .frame(width: image.size.width, height: image.size.height)
                            .padding(10)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Diagram")
                    .accessibilityHint("Opens it full screen")
                }
                .background(Palette.codeBackground, in: RoundedRectangle(cornerRadius: 8))
                .kithOverlay(isPresented: $viewing) { _ in
                    MermaidViewer(source: source, image: image)
                }
            } else {
                CodeBlock(text: source)
            }
        }
        .task(id: "\(dark)\n\(source)") {
            guard image == nil, let new = await MermaidRenderer.shared.image(source, dark: dark) else { return }
            drawn = (source, dark, new)
        }
    }
}

/// A diagram full screen: pinch to zoom, its source, and Copy.
private struct MermaidViewer: View {
    let source: String
    let image: UIImage
    @State private var showingSource = false

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader(showingSource ? "Diagram source" : "Diagram") {
                IconButton(showingSource ? "Show diagram" : "Show source",
                           systemImage: showingSource ? "photo" : "chevron.left.forwardslash.chevron.right") {
                    showingSource.toggle()
                }
                IconButton("Copy", systemImage: "doc.on.doc") { Pasteboard.copy(source) }
            }
            if showingSource {
                ScrollView([.vertical, .horizontal]) {
                    Text(source)
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(Palette.text)
                        .textSelection(.enabled)
                        .padding(16)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ZoomableImage(image: image)
            }
        }
        .background(Palette.background.ignoresSafeArea())
    }
}

/// An image that fits the screen and zooms with a pinch.
private struct ZoomableImage: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context: Context) -> ZoomView { ZoomView(image: image) }
    func updateUIView(_ view: ZoomView, context: Context) {}

    final class ZoomView: UIScrollView, UIScrollViewDelegate {
        private let imageView: UIImageView
        private var fitted = CGSize.zero

        init(image: UIImage) {
            imageView = UIImageView(image: image)
            super.init(frame: .zero)
            addSubview(imageView)
            contentSize = image.size
            delegate = self
            bouncesZoom = true
            showsHorizontalScrollIndicator = false
            showsVerticalScrollIndicator = false
            contentInsetAdjustmentBehavior = .never
        }

        required init?(coder: NSCoder) { nil }

        override func layoutSubviews() {
            super.layoutSubviews()
            if bounds.size != fitted, bounds.width > 0, bounds.height > 0, let size = imageView.image?.size {
                fitted = bounds.size
                let fit = min(1, bounds.width / size.width, bounds.height / size.height)
                minimumZoomScale = fit
                maximumZoomScale = max(4, fit)
                zoomScale = fit
            }
            center()
        }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }

        func scrollViewDidZoom(_ scrollView: UIScrollView) { center() }

        private func center() {
            let x = max(0, (bounds.width - contentSize.width) / 2)
            let y = max(0, (bounds.height - contentSize.height) / 2)
            contentInset = UIEdgeInsets(top: y, left: x, bottom: y, right: x)
        }
    }
}
