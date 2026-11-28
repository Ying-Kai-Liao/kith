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
        Group {
            if variant == .home { home } else { header }
        }
        .ignoresSafeArea(edges: !edgeToEdge ? [] : variant == .header ? .top : .all)
        .accessibilityHidden(true)
    }

    /// The ground fills the whole screen; the sky sits on top of it and fades out inside its own artwork. A mask taller
    /// than the artwork left the picture's bottom edge showing, a hard cut across the middle of the screen.
    private var home: some View {
        GeometryReader { proxy in
            let height = proxy.size.width * homeSkyScale * 1160.0 / 1179
            ZStack(alignment: .top) {
                Image(imageName, bundle: .module)
                    .resizable()
                    .scaledToFill()
                    .frame(width: proxy.size.width, height: height, alignment: .top)
                    .clipped()
                    // Night clouds sit close to the ground colour; lift them so they read.
                    .brightness(isNight ? 0.14 : 0)
                    .contrast(isNight ? 1.25 : 1.1)
                    // Day: a touch more colour so the blue and clouds read against the white page.
                    .saturation(isNight ? 1 : 1.6)
                if !isNight {
                    // Day: a soft sky-blue wash over the artwork (multiply keeps the clouds).
                    LinearGradient(colors: [Color(red: 0.72, green: 0.86, blue: 1), .clear], startPoint: .top, endPoint: .bottom)
                        .frame(height: height * 0.8)
                        .blendMode(.multiply)
                }
            }
            .frame(width: proxy.size.width, height: height, alignment: .top)
            .mask(alignment: .top) {
                LinearGradient(stops: [.init(color: .black, location: 0), .init(color: .black, location: isNight ? 0.3 : 0.5), .init(color: .clear, location: 1)],
                               startPoint: .top, endPoint: .bottom)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .background(isNight ? Palette.night : Color.white)
    }

    private var header: some View {
        ZStack(alignment: .top) {
            (isNight ? Palette.night : Color.white)
            GeometryReader { proxy in
                Image(imageName, bundle: .module)
                    .resizable()
                    .scaledToFill()
                    .frame(width: proxy.size.width, height: proxy.size.width * 600.0 / 1179, alignment: .top)
                    .clipped()
                    .contrast(isNight ? 1 : 1.1)
                    .saturation(isNight ? 1 : 1.6)
            }
            if !isNight {
                LinearGradient(colors: [Color(red: 0.72, green: 0.86, blue: 1), .clear], startPoint: .top, endPoint: .bottom)
                    .frame(height: KithSize.skyHeaderFade)
                    .blendMode(.multiply)
            }
        }
        // The strip is only its own height; the ground behind it comes from the screen.
        .frame(height: KithSize.skyHeaderFade)
        .mask(alignment: .top) {
            LinearGradient(stops: [.init(color: .black, location: 0.55), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom)
        }
    }
}
