import Testing
@testable import KithUI

@Suite struct ProfileImageTests {
    @Test func eightSkyPresetsMatchDesktop() {
        #expect(SkyPreset.all.map { "\($0.tone) \($0.x) \($0.y)" } == [
            "dawn 20 30", "dawn 70 60", "clear 15 20", "clear 60 40", "clear 85 75",
            "dusk 25 50", "dusk 75 30", "dusk 50 85",
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
