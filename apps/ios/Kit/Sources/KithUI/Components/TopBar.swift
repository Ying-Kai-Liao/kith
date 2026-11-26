import KithKit
import SwiftUI

/// The two places the top bar switches between (Work is not a place).
public enum HomeTab: String, CaseIterable, Sendable {
    case crew = "Crew"
    case feed = "Feed"
}

/// The segmented glass switch in the top bar: Crew | Feed.
public struct HomeTabSwitch: View {
    @Binding var selection: HomeTab
    @Namespace private var ns

    public init(selection: Binding<HomeTab>) {
        _selection = selection
    }

    public var body: some View {
        HStack(spacing: 2) {
            ForEach(HomeTab.allCases, id: \.self) { tab in
                Button { withAnimation(.snappy(duration: 0.2)) { selection = tab } } label: {
                    HStack(spacing: 6) {
                        Text(tab.rawValue).kithText(.topBar).foregroundStyle(Palette.ink)
                    }
                    .padding(.horizontal, 16)
                    .frame(minHeight: KithSize.topButton - 6)
                    .background {
                        if selection == tab { Capsule().fill(Palette.ink.opacity(0.09)).matchedGeometryEffect(id: "seg", in: ns) }
                    }
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(selection == tab ? [.isSelected, .isButton] : .isButton)
            }
        }
        .padding(3)
        .frame(minHeight: KithSize.topButton)
        .kithGlass(in: Capsule(), interactive: true)
    }
}

/// A round glass top button (42 pt look, 44 pt target): the picture on the left of the top bar.
public struct TopGlassButton<Content: View>: View {
    let label: String
    let action: () -> Void
    let content: Content

    public init(_ label: String, action: @escaping () -> Void, @ViewBuilder content: () -> Content) {
        self.label = label
        self.action = action
        self.content = content()
    }

    public var body: some View {
        Button(action: action) {
            content.frame(width: KithSize.topButton, height: KithSize.topButton).clipShape(Circle())
                .frame(width: 44, height: 44).contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(label))
    }
}

public extension TopGlassButton where Content == Image {
    /// A glass circle with a symbol.
    init(_ label: String, systemImage: String, action: @escaping () -> Void) {
        self.init(label, action: action) { Image(systemName: systemImage) }
    }
}

/// The new-chat button: square.and.pencil in a round glass button that opens a menu (new bot, invite, group).
public struct TopGlassMenu<Items: View>: View {
    let label: String
    let systemImage: String
    let items: Items

    public init(_ label: String, systemImage: String = "square.and.pencil", @ViewBuilder items: () -> Items) {
        self.label = label
        self.systemImage = systemImage
        self.items = items()
    }

    public var body: some View {
        Menu { items } label: {
            Image(systemName: systemImage).font(.system(size: 17, weight: .medium)).foregroundStyle(Palette.ink)
                .kithGlassCircle()
                .frame(width: 44, height: 44).contentShape(Circle())
        }
        .accessibilityLabel(Text(label))
    }
}

/// The profile picture button, top left: your sky patch (or your photo) in a ring.
public struct ProfileTopButton<Picture: View>: View {
    let action: () -> Void
    let picture: Picture

    public init(action: @escaping () -> Void, @ViewBuilder picture: () -> Picture) {
        self.action = action
        self.picture = picture()
    }

    public var body: some View {
        TopGlassButton("Account and settings", action: action) { picture }
    }
}

/// Small top bar text: Inter 600 16. No large titles.
public struct TopBarText: View {
    let text: String
    public init(_ text: String) { self.text = text }

    public var body: some View {
        Text(text).kithText(.topBar).foregroundStyle(Palette.ink).lineLimit(1)
    }
}

#Preview("Top bar") {
    @Previewable @State var tab = HomeTab.crew
    return ZStack(alignment: .top) {
        SkyBackground(.home, edgeToEdge: false)
        HStack {
            ProfileTopButton(action: {}) { SkyPatch(id: "kai", size: 38, isYou: true) }
            Spacer()
            HomeTabSwitch(selection: $tab)
            Spacer()
            TopGlassMenu("New chat") {
                Button("New bot", systemImage: "plus.square") {}
                Button("Invite a person", systemImage: "person.badge.plus") {}
            }
        }
        .padding(.horizontal, KithSpacing.gutter)
        .padding(.top, 8)
    }
    .frame(height: 120)
}
