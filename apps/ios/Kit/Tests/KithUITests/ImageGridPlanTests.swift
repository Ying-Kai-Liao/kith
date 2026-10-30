import Testing
@testable import KithKit
@testable import KithUI

private func file(_ name: String, size: Int64 = 1000) -> SharedFile {
    SharedFile(id: name, name: name, size: size, sha256: String(repeating: "a", count: 64))
}

@Test func gridShowsAtMostFourTilesAndCountsTheRest() {
    #expect(ImageGridPlan.visible(2) == 2)
    #expect(ImageGridPlan.visible(4) == 4)
    #expect(ImageGridPlan.visible(5) == 4)
    #expect(ImageGridPlan.overflow(4) == 0)
    #expect(ImageGridPlan.overflow(5) == 1)
    #expect(ImageGridPlan.overflow(9) == 5)
}

@Test func splitKeepsOversizedEmptyAndNonImageFilesAsCards() {
    let parts = ImageGridPlan.split([
        file("a.png"), file("notes.pdf"), file("huge.jpg", size: 30_000_000), file("empty.png", size: 0), file("b.HEIC"),
    ])
    #expect(parts.images.map(\.name) == ["a.png", "b.HEIC"])
    #expect(parts.others.map(\.name) == ["notes.pdf", "huge.jpg", "empty.png"])
}
