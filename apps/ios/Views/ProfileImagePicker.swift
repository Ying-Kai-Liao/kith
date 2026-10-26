import KithKit
import KithUI
import PhotosUI
import SwiftUI

/// Change the active account's profile image: a sign-in provider's picture, a Kith sky, or a photo.
/// The image lives in Clerk only; `AccountSession.setProfileImage` uploads it and reloads the user.
struct ProfileImagePicker: View {
    @Environment(AccountSession.self) private var account

    var body: some View {
        ProfileImagePickerContent(sources: account.providerImages) { await account.setProfileImage($0) }
    }
}

/// The sheet itself, with its sources and upload passed in so it can be shown with stub data.
struct ProfileImagePickerContent: View {
    let sources: [ProfileImageSource]
    /// Uploads the JPEG as the profile image; false when it failed.
    let upload: (Data) async -> Bool
    @Environment(\.dismiss) private var dismiss
    @State private var uploading: Bool
    @State private var failed: Bool
    @State private var photo: PhotosPickerItem?

    init(sources: [ProfileImageSource], uploading: Bool = false, failed: Bool = false,
         upload: @escaping (Data) async -> Bool) {
        self.sources = sources
        self.upload = upload
        _uploading = State(initialValue: uploading)
        _failed = State(initialValue: failed)
    }

    private let columns = [GridItem(.adaptive(minimum: 72), spacing: 14)]

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    if uploading {
                        HStack(spacing: 8) { ProgressView(); Text("Updating photo").font(.subheadline).foregroundStyle(Palette.secondary) }
                    } else if failed {
                        Text("Couldn't change your photo. Your current one is unchanged.")
                            .font(.subheadline).foregroundStyle(.red)
                    }
                    ForEach(Dictionary(grouping: sources, by: \.title).sorted { $0.key < $1.key }, id: \.key) { title, items in
                        section("From \(title)") {
                            ForEach(items) { item in
                                tile(label: "\(title) photo") { await send(url: item.url) } content: {
                                    AccountAvatar(url: item.url, email: nil, size: 72)
                                }
                            }
                        }
                    }
                    section("Kith defaults") {
                        ForEach(SkyPreset.all) { preset in
                            tile(label: "Kith sky") {
                                guard let data = preset.renderedJPEG() else { return failed = true }
                                await send(data: data)
                            } content: {
                                SkyPatch(preset: preset, size: 72)
                            }
                        }
                    }
                    section("Upload") {
                        PhotosPicker(selection: $photo, matching: .images) {
                            Circle().fill(Palette.panel).frame(width: 72, height: 72)
                                .overlay(Image(systemName: "photo.on.rectangle").foregroundStyle(Palette.secondary))
                        }
                        .disabled(uploading)
                        .opacity(uploading ? 0.4 : 1)
                        .accessibilityLabel("Choose from photo library")
                    }
                }
                .padding(20)
            }
            .navigationTitle("Profile photo")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .onChange(of: photo) { _, item in
                guard let item else { return }
                Task {
                    defer { photo = nil }
                    guard let raw = try? await item.loadTransferable(type: Data.self),
                          let data = Self.downscaledJPEG(raw) else { return failed = true }
                    await send(data: data)
                }
            }
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title).font(.footnote.weight(.semibold)).foregroundStyle(Palette.secondary)
            LazyVGrid(columns: columns, alignment: .leading, spacing: 14) { content() }
        }
    }

    private func tile<Content: View>(label: String, action: @escaping () async -> Void,
                                     @ViewBuilder content: () -> Content) -> some View {
        Button { Task { await action() } } label: { content().clipShape(Circle()) }
            .buttonStyle(PressScale())
            .disabled(uploading)
            .opacity(uploading ? 0.4 : 1)
            .accessibilityLabel(label)
    }

    private func send(url: URL) async {
        guard !uploading else { return }
        uploading = true
        defer { uploading = false }
        guard let (data, response) = try? await URLSession.shared.data(from: url),
              (response as? HTTPURLResponse)?.statusCode == 200,
              let jpeg = Self.downscaledJPEG(data) else { return failed = true }
        await upload(jpeg)
    }

    private func send(data: Data) async {
        guard !uploading else { return }
        uploading = true
        defer { uploading = false }
        await upload(data)
    }

    private func upload(_ data: Data) async {
        failed = false
        if await upload(data) { dismiss() } else { failed = true }
    }

    /// At most 1024px on the long side, JPEG; nil when the data isn't an image.
    static func downscaledJPEG(_ data: Data, maxSide: CGFloat = 1024) -> Data? {
        guard let image = UIImage(data: data) else { return nil }
        let scale = min(1, maxSide / max(image.size.width, image.size.height))
        let size = CGSize(width: (image.size.width * scale).rounded(), height: (image.size.height * scale).rounded())
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        return UIGraphicsImageRenderer(size: size, format: format)
            .jpegData(withCompressionQuality: 0.85) { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
    }
}

#if DEBUG
/// `-profile-picker-preview [uploading|failed]` shows the sheet with stub data, for screenshots.
enum ProfileImagePickerPreview {
    static var mode: String? {
        let args = CommandLine.arguments
        guard let i = args.firstIndex(of: "-profile-picker-preview") else { return nil }
        return args.indices.contains(i + 1) ? args[i + 1] : ""
    }

    @ViewBuilder static func view(_ mode: String) -> some View {
        ProfileImagePickerContent(
            sources: [
                ProfileImageSource(provider: "oauth_google", url: URL(string: "https://example.invalid/g.jpg")!),
                ProfileImageSource(provider: "oauth_github", url: URL(string: "https://example.invalid/h.jpg")!),
            ],
            uploading: mode == "uploading", failed: mode == "failed") { _ in false }
    }
}
#endif
