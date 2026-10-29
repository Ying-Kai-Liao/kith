import CoreGraphics
import Foundation
import ImageIO
import UIKit

/// Recolours a body with a palette pair: the greyscale `tint` map (L = position on the bot's
/// gradient, A = body alpha) becomes a 256-entry gradient from `a` to `b`, interpolated in OKLab
/// (LAYERS.md section 4). Done once per (bot, colour) and cached; never per frame. It is a CPU
/// lookup rather than CIColorMap so the map's grey values stay raw (no colour management) and
/// the result is testable.
enum LivingBotTint {
    /// The stored `avatarColor` of a palette pick is `kith:<id>`; anything else is the untouched body.
    static let prefix = "kith:"

    /// The id the drawing code carries: `<id>`, or `<id>:plain` for a flat fill.
    static func paletteId(_ stored: String?) -> String? {
        AvatarPalette.parse(stored).map { $0.plain ? $0.id + ":plain" : $0.id }
    }

    // MARK: Cache

    private final class Box { let image: CGImage?; init(_ image: CGImage?) { self.image = image } }
    nonisolated(unsafe) private static let cache: NSCache<NSString, Box> = {
        let c = NSCache<NSString, Box>()
        c.countLimit = 48
        return c
    }()

    /// The image to draw for a body: the untouched asset, or its recolour for a palette id.
    static func image(asset: String, paletteId: String?) -> CGImage? {
        let key = "\(asset)|\(paletteId ?? "")" as NSString
        if let hit = cache.object(forKey: key) { return hit.image }
        let image: CGImage?
        let plain = paletteId?.hasSuffix(":plain") == true
        let bare = paletteId.map { plain ? String($0.dropLast(6)) : $0 }
        if let bare, let rig = LivingBotData.rig(forAsset: asset), rig.tint,
           let found = LivingBotData.palette.first(where: { $0.id == bare }) {
            let pair = plain ? PalettePair(id: found.id, a: found.a, b: found.a) : found
            image = recolour(asset: asset, pair: pair, detail: rig.detail) ?? original(asset)
        } else {
            image = original(asset)
        }
        cache.setObject(Box(image), forKey: key)
        return image
    }

    static func original(_ asset: String) -> CGImage? {
        UIImage(named: asset, in: .module, with: nil)?.cgImage
    }

    // MARK: Recolour

    private struct Map { var width: Int; var height: Int; var l: [UInt8]; var a: [UInt8] }

    /// Raw grey + alpha pixels of a bundled map, nothing colour-managed.
    private static func readMap(_ name: String) -> Map? {
        guard let url = Bundle.module.url(forResource: name, withExtension: "png", subdirectory: "LivingBots"),
              let src = CGImageSourceCreateWithURL(url as CFURL, nil),
              let img = CGImageSourceCreateImageAtIndex(src, 0, nil) else { return nil }
        let w = img.width, h = img.height
        // Draw into grey and into an alpha-only context: the grey one over black would
        // multiply L by alpha, so read colour from a premultiplied RGBA and undo it.
        var px = [UInt8](repeating: 0, count: w * h * 4)
        let ok = px.withUnsafeMutableBytes { buf -> Bool in
            guard let ctx = CGContext(data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                      space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard ok else { return nil }
        var l = [UInt8](repeating: 0, count: w * h), a = l
        for i in 0..<(w * h) {
            let alpha = Int(px[i * 4 + 3])
            a[i] = UInt8(alpha)
            l[i] = alpha == 0 ? 0 : UInt8(min(255, (Int(px[i * 4]) * 255 + alpha / 2) / alpha))
        }
        return Map(width: w, height: h, l: l, a: a)
    }

    static func recolour(asset: String, pair: PalettePair, detail: Bool) -> CGImage? {
        guard let map = readMap(asset + "-tint") else { return nil }
        let lut = gradient(pair.a, pair.b, steps: 256)
        let w = map.width, h = map.height
        var out = [UInt8](repeating: 0, count: w * h * 4)
        for i in 0..<(w * h) {
            let alpha = Int(map.a[i])
            guard alpha > 0 else { continue }
            let c = lut[Int(map.l[i])]
            out[i * 4] = UInt8((Int(c.0) * alpha + 127) / 255)
            out[i * 4 + 1] = UInt8((Int(c.1) * alpha + 127) / 255)
            out[i * 4 + 2] = UInt8((Int(c.2) * alpha + 127) / 255)
            out[i * 4 + 3] = UInt8(alpha)
        }
        if detail, let mask = readMap(asset + "-detail"), mask.width == w, mask.height == h {
            // The coder's < > glyph: painted in the first colour, darker (OKLab L x 0.55, chroma kept).
            let ink = rgb(darker(Lab(hex: pair.a), 0.55))
            for i in 0..<(w * h) where mask.a[i] > 0 && out[i * 4 + 3] > 0 {
                let d = Int(mask.a[i]), alpha = Int(out[i * 4 + 3])
                for k in 0..<3 {
                    let target = Int(ink[k]) * alpha / 255
                    out[i * 4 + k] = UInt8((Int(out[i * 4 + k]) * (255 - d) + target * d + 127) / 255)
                }
            }
        }
        return out.withUnsafeMutableBytes { buf -> CGImage? in
            CGContext(data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                      space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)?.makeImage()
        }
    }

    // MARK: OKLab

    struct Lab { var l: Double, a: Double, b: Double }

    static func parse(_ hex: String) -> (Double, Double, Double) {
        let v = UInt32(hex.dropFirst(), radix: 16) ?? 0
        return (Double((v >> 16) & 0xFF) / 255, Double((v >> 8) & 0xFF) / 255, Double(v & 0xFF) / 255)
    }

    private static func lin(_ c: Double) -> Double { c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4) }
    private static func gamma(_ c: Double) -> Double { c <= 0.0031308 ? 12.92 * c : 1.055 * pow(c, 1 / 2.4) - 0.055 }

    static func oklab(_ r: Double, _ g: Double, _ b: Double) -> Lab {
        let r = lin(r), g = lin(g), b = lin(b)
        let l = cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
        let m = cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
        let s = cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
        return Lab(l: 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
                   a: 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
                   b: 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s)
    }

    static func srgb(_ c: Lab) -> (Double, Double, Double) {
        let l = pow(c.l + 0.3963377774 * c.a + 0.2158037573 * c.b, 3)
        let m = pow(c.l - 0.1055613458 * c.a - 0.0638541728 * c.b, 3)
        let s = pow(c.l - 0.0894841775 * c.a - 1.2914855480 * c.b, 3)
        func clamp(_ v: Double) -> Double { min(1, max(0, gamma(v))) }
        return (clamp(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
                clamp(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
                clamp(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s))
    }

    private static func rgb(_ c: Lab) -> [UInt8] {
        let (r, g, b) = srgb(c)
        return [UInt8((r * 255).rounded()), UInt8((g * 255).rounded()), UInt8((b * 255).rounded())]
    }

    private static func darker(_ c: Lab, _ factor: Double) -> Lab { Lab(l: c.l * factor, a: c.a, b: c.b) }

    /// 256 sRGB entries from `a` (0) to `b` (255), interpolated in OKLab.
    static func gradient(_ a: String, _ b: String, steps: Int) -> [(UInt8, UInt8, UInt8)] {
        let (ar, ag, ab) = parse(a), (br, bg, bb) = parse(b)
        let x = oklab(ar, ag, ab), y = oklab(br, bg, bb)
        return (0..<steps).map { i in
            let t = Double(i) / Double(steps - 1)
            let c = rgb(Lab(l: x.l + (y.l - x.l) * t, a: x.a + (y.a - x.a) * t, b: x.b + (y.b - x.b) * t))
            return (c[0], c[1], c[2])
        }
    }
}

extension LivingBotTint.Lab {
    init(hex: String) {
        let (r, g, b) = LivingBotTint.parse(hex)
        self = LivingBotTint.oklab(r, g, b)
    }
}
