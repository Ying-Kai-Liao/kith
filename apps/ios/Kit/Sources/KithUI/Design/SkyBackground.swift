import KithKit
import SwiftUI

/// Registers the fonts that ship in KithUI's resources. Call once at app launch; repeat calls do nothing.
/// Widgets and the notification extension never call it and keep the system fonts.
public enum KithUIFonts {
    public static func register() { KithFonts.register(from: .module) }
    static var resources: Bundle { .module }
}

/// The Kith morning sky (night: the same clouds at night), drawn from the top of the screen into the ground
/// colour. Put it behind the content, outside the scroll view, so it stays still while the list scrolls.
public struct SkyBackground: View {
    public enum Variant: Sendable {
        /// Home: fills the top, fades into the ground by mid-screen.
        case home
        /// Agent chat: a short strip behind the top bar, gone by about 170 pt.
        case header
    }

    private let variant: Variant
    private let night: Bool?
    private let edgeToEdge: Bool
    @Environment(\.colorScheme) private var scheme

    /// `night` forces a variant; nil follows the appearance.
    /// `edgeToEdge: false` keeps it inside the safe area (a preview card, not a screen background).
    public init(_ variant: Variant = .home, night: Bool? = nil, edgeToEdge: Bool = true) {
        self.variant = variant
        self.night = night
        self.edgeToEdge = edgeToEdge
    }

    /// The home sky is 10% taller than the artwork's natural crop, so more of the clouds show.
    private let homeSkyScale: CGFloat = 1.1

    private var isNight: Bool { night ?? (scheme == .dark) }

    private var imageName: String {
        switch variant {
        case .home: isNight ? "sky-home-night" : "sky-home-day"
        case .header: isNight ? "sky-header-night" : "sky-header-day"
        }
    }

    public var body: some View {
        ZStack(alignment: .top) {
            (isNight ? Palette.night : Color.white)
            // Sized from the width offered, whatever height the parent gives: the image is never squeezed.
            GeometryReader { proxy in
                Image(imageName, bundle: .module)
                    .resizable()
                    .scaledToFill()
                    .frame(width: proxy.size.width, height: proxy.size.width * (variant == .home ? homeSkyScale * 1160.0 / 1179 : 600.0 / 1179), alignment: .top)
                    .clipped()
                    // Night clouds sit close to the ground colour; lift them so they read.
                    .brightness(variant == .home && isNight ? 0.14 : 0)
                    .contrast(variant == .home && isNight ? 1.25 : (isNight ? 1 : 1.1))
                    // Day: a touch more colour so the blue and clouds read against the white page.
                    .saturation(isNight ? 1 : 1.6)
            }
            if !isNight {
                // Day: a soft sky-blue wash over the artwork (multiply keeps the clouds), so the sky reads against the white page.
                // The home wash scales with the screen (a fixed 330 pt ended a third of the way down a Pro Max and looked like a cut).
                LinearGradient(colors: [Color(red: 0.72, green: 0.86, blue: 1), .clear], startPoint: .top, endPoint: variant == .home ? UnitPoint(x: 0.5, y: 0.55) : .bottom)
                    .frame(height: variant == .home ? nil : KithSize.skyHeaderFade)
                    .blendMode(.multiply)
            }
        }
        // The strip is only its own height; the ground behind it comes from the screen.
        .frame(height: variant == .header ? KithSize.skyHeaderFade : nil)
        .mask(alignment: .top) {
            if variant == .header {
                LinearGradient(stops: [.init(color: .black, location: 0.55), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom)
            } else {
                // Full strength at the top, clear by about half the height, as in the home mock.
                LinearGradient(stops: [.init(color: .black, location: 0), .init(color: .black, location: isNight ? 0.2 : 0.3), .init(color: .clear, location: isNight ? 0.55 : 0.7)],
                               startPoint: .top, endPoint: .bottom)
            }
        }
        .ignoresSafeArea(edges: !edgeToEdge ? [] : variant == .header ? .top : .all)
        .accessibilityHidden(true)
    }
}
