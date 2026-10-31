import KithKit
import KithUI
import SwiftUI
import VisionKit

/// Add a computer running Kith: scan its code first, with install one row away.
struct PairingView<Leading: View>: View {
    /// Adding another computer from the computers sheet: has a close button; closes once paired.
    var inModal = false
    /// Finish onboarding without pairing; modal pairing simply closes when skipped.
    var onSkip: (() -> Void)? = nil
    /// Top-left on the scan page (back to the welcome, the account switcher).
    @ViewBuilder var leading: Leading

    private enum Step { case install, scan }
    @State private var step = PairingView.firstStep
    @State private var os = ComputerOS.mac
    @Environment(\.dismissModal) private var dismissModal
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 0) {
            ScreenHeader {
                if step == .install { BackButton { go(.scan) } } else { leading }
            } title: {
                Text(step == .install ? "Install Kith" : "Add a Mac")
                    .kithText(.topBar)
                    .foregroundStyle(Palette.text)
                    .accessibilityAddTraits(.isHeader)
            } trailing: {
                if inModal { IconButton("Close", systemImage: "xmark") { dismissModal() } }
            }

            ZStack {
                switch step {
                case .install:
                    InstallPage(os: $os) { go(.scan) }
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                case .scan:
                    ScanPage(os: os, inModal: inModal, install: { go(.install) }) { if inModal { dismissModal() } }
                        .transition(.move(edge: .leading).combined(with: .opacity))
                }
            }
            .frame(maxHeight: .infinity)
            .clipped()

            if step == .scan && !inModal {
                Button {
                    withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { onSkip?() }
                } label: {
                    Text("Skip for now").kithText(.topBar).frame(maxWidth: .infinity, minHeight: 50)
                }
                .buttonStyle(.line)
                .accessibilityHint("Add a Mac later from Settings, Account.")
                .padding(.horizontal, 24)
                .padding(.bottom, 12)
                .transition(.opacity)
            }
        }
        .background { SkyBackground() }
    }

    private func go(_ next: Step) {
        withAnimation(Motion.reduced(.spring(duration: 0.45, bounce: 0), reduceMotion)) { step = next }
    }

    /// `-kithDemoScreens pairing-install` (CI screenshots) opens on the install page.
    private static var firstStep: Step {
        #if DEBUG
        if DemoScreens.name == "pairing-install" { return .install }
        #endif
        return .scan
    }
}

extension PairingView where Leading == EmptyView {
    init(inModal: Bool) { self.init(inModal: inModal) { EmptyView() } }
}

private enum ComputerOS: Hashable { case mac, linux }

/// A tappable card row: icon, two lines, chevron.
private struct CardRow: View {
    let title: String
    let detail: String
    let systemImage: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 14) {
                Image(systemName: systemImage).font(.title3).foregroundStyle(Palette.text).frame(width: 28)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).kithText(.body).foregroundStyle(Palette.text)
                    Text(detail).kithText(.body).foregroundStyle(Palette.mutedOnSky)
                }
                .multilineTextAlignment(.leading)
                Spacer(minLength: 8)
                Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(Palette.tertiary)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
            .frame(maxWidth: .infinity)
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(PressScale())
    }
}

// MARK: - Install

private struct InstallPage: View {
    @Binding var os: ComputerOS
    let next: () -> Void
    @Environment(\.openURL) private var openURL

    /// The README's release install: one binary that carries `kith serve`.
    private let installCommand = "curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh"
    private let guide = URL(string: "https://github.com/Ying-Kai-Liao/kith#readme")!

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    SegmentedChoice(selection: $os, options: [(.mac, "Mac"), (.linux, "Linux")])

                    VStack(alignment: .leading, spacing: 22) {
                        InstallStep(number: 1, title: "Install Kith", note: "In Terminal on your computer.", command: installCommand)
                        InstallStep(number: 2, title: "Start the host",
                                    note: os == .mac
                                        ? "It keeps running in the background through launchd."
                                        : "It runs as a systemd user service. Run loginctl enable-linger $USER to keep it running after logout.",
                                    command: "kith serve start")
                        InstallStep(number: 3, title: "Show the pairing code",
                                    note: os == .mac ? "Or click Kith in the menu bar, then Pair iPhone." : nil,
                                    command: "~/.kith/serve/bin/kith-serve pair")
                    }
                    .padding(18)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.surface, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .animation(Motion.layout, value: os)

                    // Copied here, pasted on the computer (Universal Clipboard or a message to yourself).
                    Text("Copy here, paste on the computer: Universal Clipboard, or send it to yourself.")
                        .kithText(.body)
                        .foregroundStyle(Palette.mutedOnSky)
                        .padding(.horizontal, 4)

                    CardRow(title: "Setup guide", detail: "github.com/Ying-Kai-Liao/kith", systemImage: "book.closed") {
                        openURL(guide)
                    }
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 16)
            }

            Button(action: next) {
                Text("Scan the code").kithText(.topBar).frame(maxWidth: .infinity, minHeight: 50)
            }
            .buttonStyle(.ink)
            .padding(.horizontal, 24)
            .padding(.bottom, 12)
        }
    }
}

private struct InstallStep: View {
    let number: Int
    let title: String
    let note: String?
    let command: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Text("\(number)")
                .font(.system(size: 12, weight: .bold))
                .foregroundStyle(Palette.background)
                .frame(width: 22, height: 22)
                .background(Palette.text, in: Circle())
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 6) {
                Text(title).font(.body.weight(.semibold)).foregroundStyle(Palette.text)
                if let note { Text(note).kithText(.body).foregroundStyle(Palette.mutedOnSky) }
                CommandBlock(command).padding(.top, 2)
            }
        }
        .accessibilityElement(children: .contain)
    }
}

/// A terminal command with a copy button.
private struct CommandBlock: View {
    let command: String
    @State private var copied = false

    init(_ command: String) { self.command = command }

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Text(command)
                .font(.footnote.monospaced())
                .foregroundStyle(Palette.text)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 6)
                .contentTransition(.opacity)
            IconButton(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc") {
                UIPasteboard.general.string = command
                withAnimation(Motion.morph) { copied = true }
                Task {
                    try? await Task.sleep(for: .seconds(1.5))
                    withAnimation(Motion.morph) { copied = false }
                }
            }
            .contentTransition(.symbolEffect(.replace))
        }
        .padding(.leading, 14)
        .padding(.trailing, 4)
        .padding(.vertical, 4)
        .background(Palette.background, in: RoundedRectangle(cornerRadius: 12))
        .animation(Motion.fade, value: command)
    }
}

// MARK: - Scan

/// Who was just paired, for the success cover.
private struct PairedSummary {
    let name: String
    let agents: Int
    let portraits: [String]
}

private struct ScanPage: View {
    let os: ComputerOS
    let inModal: Bool
    let install: () -> Void
    let paired: () -> Void
    @Environment(AppStore.self) private var app
    @Environment(AccountSession.self) private var account
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.openURL) private var openURL
    @State private var pairing = false
    @State private var pasted = ""
    @State private var error: String?
    @State private var done: PairedSummary?
    /// Bumped after a failed pair so the scanner starts looking again.
    @State private var attempt = 0
    @State private var tailscaleOn = Tailscale.isConnected

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(spacing: 10) {
                    Text(os == .mac
                         ? "On your Mac, click Kith in the menu bar, then Pair iPhone."
                         : "On your computer, run this in a terminal:")
                        .kithText(.body)
                        .foregroundStyle(Palette.mutedOnSky)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity)
                    if os == .linux { CommandBlock("~/.kith/serve/bin/kith-serve pair") }
                }

                viewfinder

                if let error {
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: "exclamationmark.triangle").foregroundStyle(Palette.danger)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(error).font(.system(size: 14, weight: .semibold)).foregroundStyle(Palette.danger)
                            // The usual cause: the phone has no road to the computer yet.
                            Text("Check that this iPhone and the computer are on the same Wi-Fi or both on Tailscale, or that “Reach from anywhere” is on in Kith on the computer.")
                                .kithText(.secondary)
                                .foregroundStyle(Palette.mutedOnSky)
                        }
                    }
                    .padding(14)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.surface, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .transition(.opacity)
                }

                HStack {
                    TextField("Or paste a \(AppEnvironment.current.urlScheme)://pair link", text: $pasted)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.callout.monospaced())
                        .onSubmit { pair(pasted) }
                    IconButton("Pair", systemImage: "arrow.right") { pair(pasted) }
                        .disabled(pasted.isEmpty || pairing)
                }
                .padding(.leading, 14)
                .padding(.trailing, 4)
                .padding(.vertical, 4)
                .background(Palette.surface, in: RoundedRectangle(cornerRadius: 12))

                CardRow(title: "Kith isn't on your computer yet", detail: "Install it on a Mac or Linux, then come back",
                        systemImage: "laptopcomputer.and.arrow.down", action: install)

                // Signed in, account computers show up and ask for access themselves.
                if !inModal && account.isConfigured && !account.isSignedIn {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Or sign in").font(.footnote.weight(.semibold)).foregroundStyle(Palette.mutedOnSky).textCase(.uppercase)
                        SignInButtons()
                        Text("Signed in, the Macs on your account show up without a code.")
                            .kithText(.secondary)
                            .foregroundStyle(Palette.mutedOnSky)
                    }
                }

                // How it connects is never a choice here: every paired computer gets every route.
                VStack(alignment: .leading, spacing: 8) {
                    Label("Connects on Wi-Fi, over Tailscale, or from anywhere through Cloudflare, on its own. End-to-end encrypted.",
                          systemImage: "lock.fill")
                    if tailscaleOn {
                        Label("Tailscale is on: Kith connects over it directly.", systemImage: "checkmark.circle.fill")
                    } else {
                        Button { openURL(Tailscale.downloadURL) } label: {
                            Label("Use Tailscale? Direct and faster away from home.", systemImage: "arrow.up.right")
                        }
                        .buttonStyle(PressScale())
                    }
                }
                .kithText(.secondary)
                .foregroundStyle(Palette.tertiary)
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        // Coming back from the Tailscale app: show whether it's connected now.
        .onChange(of: scenePhase) { _, phase in if phase == .active { tailscaleOn = Tailscale.isConnected } }
        #if DEBUG
        .task {
            // CI shots: `-kithDemoScreens pairing-success` / `pairing-error`.
            switch DemoScreens.name {
            case "pairing-success":
                done = PairedSummary(name: "Studio Mac", agents: 3, portraits: ["crew-builder", "crew-designer", "crew-lab"])
            case "pairing-error":
                error = "That pairing code has expired or was already used."
            default: break
            }
        }
        #endif
    }

    /// The camera, right on the page: no extra sheet between the instruction and the scan.
    private var viewfinder: some View {
        ZStack {
            if DataScannerViewController.isSupported {
                QRScanner { code in pair(code) }
                    .id(attempt)
            } else {
                VStack(spacing: 10) {
                    Image(systemName: "qrcode.viewfinder").font(.system(size: 44, weight: .light))
                    Text("Camera not available. Paste the link below.").kithText(.secondary)
                }
                .foregroundStyle(Palette.tertiary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Palette.surface)
            }
            CornerBrackets().padding(48).allowsHitTesting(false)
            if pairing {
                Palette.background.opacity(0.7)
                Spinner(size: 28)
            }
            if let done { PairedCover(summary: done, reduceMotion: reduceMotion) }
        }
        .aspectRatio(1, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
        .animation(Motion.fade, value: pairing)
    }

    private func pair(_ text: String) {
        // The scanner fires once per code; this also covers a paste or a second code while one is in flight.
        guard !pairing, done == nil else { return }
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        // A pasted or scanned https invite link joins that computer, as opening it from outside the app does.
        if let url = URL(string: trimmed), InviteLink.isPageLink(url) {
            pasted = ""
            app.open(url)
            return
        }
        let p: Pairing
        do {
            p = try Pairing.parse(trimmed)
        } catch {
            show(error)
            return
        }
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { error = nil }
        pairing = true
        Task {
            defer { pairing = false }
            do {
                let computer = try await app.pair(p)
                pasted = ""
                let bots = app.accounts.store(for: computer.id)?.bots.values.sorted { $0.id < $1.id } ?? []
                withAnimation(Motion.reduced(Motion.layout, reduceMotion)) {
                    done = PairedSummary(name: computer.name, agents: bots.count,
                                         portraits: bots.prefix(4).compactMap { CrewAvatar.asset(for: $0) })
                }
                try? await Task.sleep(for: .seconds(reduceMotion ? 0.6 : 1.2))
                paired()
            } catch {
                show(error)
            }
        }
    }

    private func show(_ error: Error) {
        attempt += 1
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { self.error = error.localizedDescription }
    }
}

/// Four white corner brackets over the camera.
private struct CornerBrackets: View {
    var body: some View {
        GeometryReader { proxy in
            let arm: CGFloat = 34
            Path { p in
                let w = proxy.size.width, h = proxy.size.height
                for (x, y, dx, dy) in [(0, 0, 1, 1), (w, 0, -1, 1), (0, h, 1, -1), (w, h, -1, -1)] as [(CGFloat, CGFloat, CGFloat, CGFloat)] {
                    p.move(to: CGPoint(x: x + dx * arm, y: y))
                    p.addLine(to: CGPoint(x: x, y: y))
                    p.addLine(to: CGPoint(x: x, y: y + dy * arm))
                }
            }
            .stroke(Color.white, style: StrokeStyle(lineWidth: 3, lineCap: .round, lineJoin: .round))
        }
    }
}

/// Over the viewfinder once paired.
private struct PairedCover: View {
    let summary: PairedSummary
    let reduceMotion: Bool
    @State private var shown = false

    var body: some View {
        ZStack {
            Rectangle().fill(.ultraThinMaterial)
            Color(hex: 0x14161B).opacity(0.55)
            VStack(spacing: 10) {
                Image(systemName: "checkmark.circle").font(.system(size: 46, weight: .light))
                Text("Paired with \(summary.name)").font(.system(size: 17, weight: .semibold)).multilineTextAlignment(.center)
                if summary.agents > 0 {
                    Text(summary.agents == 1 ? "1 agent is ready" : "\(summary.agents) agents are ready").kithText(.body).opacity(0.8)
                    HStack(spacing: -6) {
                        ForEach(summary.portraits, id: \.self) { CharacterAvatar(portrait: $0, size: 30) }
                    }
                }
            }
            .foregroundStyle(.white)
            .padding(24)
            .scaleEffect(shown || reduceMotion ? 1 : 0.92)
            .opacity(shown ? 1 : 0)
        }
        .accessibilityElement(children: .combine)
        .onAppear { withAnimation(Motion.reduced(.spring(duration: 0.4, bounce: 0.15), reduceMotion)) { shown = true } }
    }
}

/// VisionKit live QR scanner.
struct QRScanner: UIViewControllerRepresentable {
    let onCode: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let vc = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .fast,
            isHighlightingEnabled: true
        )
        vc.delegate = context.coordinator
        try? vc.startScanning()
        return vc
    }

    func updateUIViewController(_ vc: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(onCode: onCode) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onCode: (String) -> Void
        private var done = false

        init(onCode: @escaping (String) -> Void) { self.onCode = onCode }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            for case let .barcode(code) in items {
                if let value = code.payloadStringValue, value.hasPrefix(AppEnvironment.current.urlScheme + "://"), !done {
                    done = true
                    scanner.stopScanning()
                    onCode(value)
                }
            }
        }
    }
}
