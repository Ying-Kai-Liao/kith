import Foundation
import KithKit
import Testing
@testable import KithUI

private func decode<T: Decodable>(_ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

@Test func personListKeepsOnlyThePersonsFiles() throws {
    let list: HqPersonMemory = try decode("""
    {"agent":[{"name":"people/sam/a.md","shared":false,"size":3,"modified":1},
              {"name":"people/sam/z/b.md","shared":false,"size":3,"modified":1},
              {"name":"people/jo/c.md","shared":false,"size":3,"modified":1}],"shared":[]}
    """)
    #expect(list.supported)
    #expect(list.files(of: "sam").map(\.name) == ["people/sam/a.md", "people/sam/z/b.md"])
    #expect(list.files(of: "nobody").isEmpty)
}

@Test func anOlderHostThatIgnoresThePersonFilterShowsNothing() throws {
    let list: HqPersonMemory = try decode("""
    {"agent":[{"name":"MEMORY.md","shared":false,"size":3,"modified":1},
              {"name":"people/sam/a.md","shared":false,"size":3,"modified":1}],
     "shared":[{"name":"user.md","shared":true,"size":3,"modified":1}]}
    """)
    #expect(!list.supported)
    #expect(list.files(of: "sam").isEmpty)
}

@Test func forgetAnswerDecodesVersions() throws {
    let done: HqPersonForget = try decode("""
    {"person":"sam","removed":[{"name":"people/sam/a.md","version":{"id":7,"at":1,"author":"kai","cause":"delete","deleted":true}}]}
    """)
    #expect(done.removed.map(\.name) == ["people/sam/a.md"])
    #expect(done.removed.first?.version == 7)
}

@MainActor private final class Calls {
    var files: [HqMemoryFileEntry]
    var forgets: [String?] = []
    var restores: [String] = []

    init(names: [String]) {
        files = names.map { HqMemoryFileEntry(name: "people/sam/\($0)", shared: false) }
    }

    func model() -> PersonMemoryModel {
        PersonMemoryModel(
            personId: "sam",
            list: { HqPersonMemory(files: self.files) },
            forget: { name in
                self.forgets.append(name)
                let hit = self.files.filter { name == nil || $0.name == "people/sam/\(name!)" }
                self.files.removeAll { f in hit.contains { $0.name == f.name } }
                return HqPersonForget(person: "sam", removed: hit.map { .init(name: $0.name, version: 4) })
            },
            restore: { name, version in
                self.restores.append("\(name)@\(version)")
                self.files.append(HqMemoryFileEntry(name: name, shared: false))
            })
    }
}

@MainActor @Test func forgetOneFileThenUndo() async {
    let calls = Calls(names: ["a.md", "b.md"])
    let m = calls.model()
    await m.load()
    #expect(m.visible && m.files.count == 2)
    await m.forget(file: m.files[0])
    #expect(calls.forgets == ["a.md"])
    #expect(m.files.map(\.name) == ["people/sam/b.md"])
    #expect(m.undo?.label == "A")
    await m.restore()
    #expect(calls.restores == ["people/sam/a.md@4"])
    #expect(m.files.count == 2)
    #expect(m.undo == nil)
}

@MainActor @Test func forgetEverythingSendsNoNameAndUndoBringsAllBack() async {
    let calls = Calls(names: ["a.md", "b.md"])
    let m = calls.model()
    await m.load()
    await m.forgetAll(name: "Sam")
    #expect(calls.forgets.count == 1 && calls.forgets[0] == nil)
    #expect(m.files.isEmpty)
    #expect(m.undo?.removed.count == 2)
    await m.restore()
    #expect(m.files.count == 2)
}

@MainActor @Test func aSecondForgetForNothingLeavesNoUndo() async {
    let m = Calls(names: []).model()
    await m.load()
    await m.forgetAll(name: "Sam")
    #expect(m.undo == nil)
}

@MainActor @Test func unsupportedHostHidesTheRow() async {
    let m = PersonMemoryModel(personId: "sam", list: { throw HqMemoryUnsupported() },
                              forget: { _ in HqPersonForget(person: "sam", removed: []) }, restore: { _, _ in })
    await m.load()
    #expect(!m.visible)
}

@Test func notesReadAsTitlesNotFiles() {
    #expect(PersonMemorySheet.noteTitle("people/sam/trip-plans.md") == "Trip plans")
    #expect(PersonMemorySheet.noteTitle("people/sam/z/recipes.md") == "Recipes")
}
