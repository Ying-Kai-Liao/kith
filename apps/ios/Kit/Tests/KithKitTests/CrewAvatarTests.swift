import Foundation
import Testing
import UIKit
@testable import KithKit

@Test func autoShowsTheMatchingCrewPortraitElseTheHammerBot() {
    #expect(CrewAvatar.asset(source: "auto", id: "b1", name: "House", isGroup: false) == "crew-house")
    #expect(CrewAvatar.asset(source: "auto", id: "hq-sound", name: "Helper", isGroup: false) == "crew-sound")
    #expect(CrewAvatar.asset(source: "auto", id: "b2", name: "seo", isGroup: false) == "crew-builder")
}

@Test func aHostWithoutAvatarSourceMeansAuto() throws {
    let bot = try JSONDecoder().decode(Bot.self, from: Data(#"{"id":"b","name":"Sam"}"#.utf8))
    #expect(bot.avatarSource == "auto")
    #expect(bot.avatarColor == "")
    #expect(CrewAvatar.asset(for: bot) == "crew-builder")
}

@Test func anExplicitImageWinsAndTheOldShapeSourceMeansAuto() {
    #expect(CrewAvatar.asset(source: "crew:video", id: "b", name: "house", isGroup: false) == "crew-video")
    #expect(CrewAvatar.asset(source: "spare:gradcap", id: "b", name: "Sam", isGroup: false) == "spare-gradcap")
    #expect(CrewAvatar.asset(source: "shape", id: "b", name: "house", isGroup: false) == "crew-house")
    #expect(CrewAvatar.asset(source: "shape", id: "b", name: "Sam", isGroup: false) == "crew-builder")
}

@Test func groupsKeepTheirOwnAvatarAndUnknownImagesFallBackToAuto() {
    #expect(CrewAvatar.asset(source: "auto", id: "g", name: "Room", isGroup: true) == nil)
    #expect(CrewAvatar.asset(source: "spare:nope", id: "b", name: "Sam", isGroup: false) == "crew-builder")
    #expect(CrewAvatar.asset(source: "future:thing", id: "b", name: "Sam", isGroup: false) == "crew-builder")
}

@Test func onlyAKithPrefixedPaletteIdRecolours() {
    #expect(AvatarPalette.pick("kith:coral") == "coral")
    #expect(AvatarPalette.pick("kith:dawn") == "dawn")
    // Old ids overlap palette ids (blue, green, cyan, violet) and must stay the original body.
    for old in ["blue", "green", "cyan", "violet", "black", "brown", "red", "orange", "gray", "magenta", "", "kith:", "kith:nope", "KITH:coral"] {
        #expect(AvatarPalette.pick(old) == nil, "\(old)")
    }
    #expect(AvatarPalette.pick(nil) == nil)
}

@Test func plainIsAFlatFillOfTheFirstColourAndOldValuesStayAsTheyWere() throws {
    #expect(AvatarPalette.parse("kith:coral")?.plain == false)
    #expect(AvatarPalette.parse("kith:coral:plain")?.id == "coral")
    #expect(AvatarPalette.parse("kith:coral:plain")?.plain == true)
    for odd in ["", "blue", "kith:", "kith:nope:plain", "kith:coral:flat", "coral:plain", "kith:coral:plain:plain"] {
        #expect(AvatarPalette.parse(odd) == nil, "\(odd)")
    }
    let swatch = try #require(AvatarPalette.colors.first { $0.id == "coral" })
    #expect(swatch.stored(plain: true) == "kith:coral:plain")
    #expect(swatch.stored(plain: false) == "kith:coral")
    #expect(AvatarPalette.pick("kith:coral:plain") == "coral")
    #expect(AvatarPalette.isPlain("kith:coral:plain") && !AvatarPalette.isPlain("kith:coral") && !AvatarPalette.isPlain(nil))
    // The badge lookup strips the prefix and the mode.
    #expect(AvatarPalette.color("kith:blue:plain") == AvatarPalette.color("kith:blue"))
    #expect(LivingBotTint.paletteId("kith:coral:plain") == "coral:plain")
    #expect(LivingBotTint.paletteId("kith:coral") == "coral")
    // A plain body is its own image, not the gradient one.
    let gradient = try #require(LivingBotTint.image(asset: "crew-builder", paletteId: "coral"))
    let flat = try #require(LivingBotTint.image(asset: "crew-builder", paletteId: "coral:plain"))
    #expect(gradient !== flat)
}

@Test func theSwiftPaletteMatchesBrandingsEyesJson() {
    let json = LivingBotData.palette
    #expect(json.count == 15)
    #expect(AvatarPalette.colors.map(\.id) == json.map { $0.id ?? "" })
    for (swatch, pair) in zip(AvatarPalette.colors, json) {
        #expect(String(format: "#%06X", swatch.a) == pair.a.uppercased())
        #expect(String(format: "#%06X", swatch.b) == pair.b.uppercased())
    }
}

@Test func everyCrewSpareAndTheCubeHasEyesAndBodyArt() throws {
    let assets = CrewAvatar.names.map { "crew-" + $0 } + CrewAvatar.spares.map { "spare-" + $0 } + ["kith-cube"]
    #expect(assets.count == 25)
    for asset in assets {
        let key = LivingBotData.rigKey(asset)
        let rig = try #require(LivingBotData.rigs[key], "\(asset)")
        #expect(rig.eyes.count == 2)
        #expect(LivingBotTint.original(asset) != nil, "\(asset) body")
        if rig.tint { #expect(LivingBotTint.image(asset: asset, paletteId: "coral") != nil, "\(asset) tint") }
    }
    #expect(LivingBotData.rigs["kith"]?.group != nil)
}

@Test func recolouringWithABotsOwnColoursReproducesItsBody() throws {
    let rig = try #require(LivingBotData.rigs["lab"])
    let colors = try #require(rig.colors)
    let tinted = try #require(LivingBotTint.recolour(asset: "crew-lab", pair: colors, detail: false))
    let original = try #require(LivingBotTint.original("crew-lab"))
    func bytes(_ img: CGImage) -> [UInt8] {
        var px = [UInt8](repeating: 0, count: 64 * 64 * 4)
        px.withUnsafeMutableBytes { buf in
            let ctx = CGContext(data: buf.baseAddress, width: 64, height: 64, bitsPerComponent: 8, bytesPerRow: 256,
                                space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            ctx.interpolationQuality = .high
            ctx.draw(img, in: CGRect(x: 0, y: 0, width: 64, height: 64))
        }
        return px
    }
    let a = bytes(tinted), b = bytes(original)
    let diff = zip(a, b).map { abs(Int($0) - Int($1)) }.reduce(0, +)
    #expect(Double(diff) / Double(a.count) < 12, "mean channel difference \(Double(diff) / Double(a.count))")
}
