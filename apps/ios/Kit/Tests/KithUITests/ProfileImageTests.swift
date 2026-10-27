import Testing
@testable import KithUI

@Suite struct ProfileImageTests {
    @Test func eightSkyPresetsMatchDesktop() {
        #expect(SkyPreset.all.map { "\($0.name) \(String($0.top, radix: 16, uppercase: true)) \(String($0.bottom, radix: 16, uppercase: true)) \($0.zoom) \($0.x) \($0.y) \($0.cloud)" } == [
            "night 50A24 1B2F7A 3 0 0 30", "golden E8801A FFD27A 3 100 0 55", "dusk 4A2F98 F07A5A 4 90 10 50",
            "dawn 6FA0E8 F8B890 4 5 15 55", "noon F4FC8 4FA8F2 3 80 40 60", "overcast 58626E AEB6BE 2 40 0 75",
            "twilight 141C5E C8409A 4 10 30 45", "mint 1E8A78 BDEBD0 3 85 70 60",
        ])
        #expect(Set(SkyPreset.all.map(\.id)).count == 8)
    }

    @Test func providerImagesKeepOnlyHttpsPictures() {
        let sources = ProfileImageSource.sources(from: [
            ("oauth_google", "https://img.example/g.png"), ("github", nil),
            ("oauth_github", ""), ("google", "http://img.example/x.png"), ("github", "https://img.example/h.png"),
        ])
        #expect(sources.map(\.title) == ["Google", "GitHub"])
        #expect(ProfileImageSource.sources(from: []).isEmpty)
    }
}
