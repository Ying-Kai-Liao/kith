import Testing
@testable import KithUI

@Suite struct ProfileImageTests {
    @Test func eightSkyPresetsMatchDesktop() {
        #expect(SkyPreset.all.map { "\($0.name) \(String($0.top, radix: 16, uppercase: true)) \(String($0.bottom, radix: 16, uppercase: true)) \($0.zoom) \($0.x) \($0.y) \($0.cloud)" } == [
            "night B1538 2C468F 3 0 0 45", "golden F29A2E FFDD94 3 100 0 60", "dusk 5A3FA8 F08A6A 4 90 10 55",
            "dawn 4F86E0 F5A878 4 5 15 60", "noon 1F5FD0 6DB8F5 4 62 25 70", "overcast 6F7A87 C9D0D6 2 40 0 85",
            "twilight 1C2468 C8509A 4 10 30 50", "mint 2E9E8A D3F2DE 4 55 5 65",
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
