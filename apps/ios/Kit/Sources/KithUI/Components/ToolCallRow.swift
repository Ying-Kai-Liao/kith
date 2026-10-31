import KithKit
import SwiftUI

/// One tool call as one line: "Edited kith-sessions.py", the result on the right.
public struct ToolCallRowModel: Equatable, Sendable {
    public enum Result: Equatable, Sendable {
        case text(String)
        /// Shown as a red word.
        case error(String)
    }

    public var verb: String
    public var object: String
    /// nil: still running.
    public var result: Result?
    /// What the sheet shows; a row with neither is not tappable.
    public var command: String?
    public var output: String?
    /// A `kith ask` row's id: the row opens the handoff's peek sheet (owner only) in place of the command and output.
    public var askId: String?

    public init(verb: String, object: String, result: Result? = nil, command: String? = nil, output: String? = nil, askId: String? = nil) {
        self.verb = verb
        self.object = object
        self.result = result
        self.command = command
        self.output = output
        self.askId = askId
    }

    var hasDetail: Bool { command != nil || output != nil }
}

/// 44 pt, radius 10, panel fill. Tap opens the stock sheet with the command and output.
public struct ToolCallRow: View {
    let model: ToolCallRowModel
    @State private var showSheet = false
    @State private var showPeek = false
    /// Absent in previews and the gallery; only the owner's store opens a handoff.
    @Environment(BotStore.self) private var store: BotStore?

    public init(_ model: ToolCallRowModel) { self.model = model }

    public var body: some View {
        if let id = model.askId, let store, store.isOwner {
            Button { showPeek = true } label: { row }
                .buttonStyle(.plain)
                .kithSheet(isPresented: $showPeek) { HandoffSheet(store: store, id: id).presentationDetents([.medium, .large]) }
                .accessibilityAddTraits(.isButton)
                .accessibilityHint(Text("Opens the handoff"))
        } else if model.hasDetail {
            Button { showSheet = true } label: { row }
                .buttonStyle(.plain)
                .sheet(isPresented: $showSheet) {
                    ToolDetailSheet(model: model).presentationDetents([.medium, .large])
                }
                .accessibilityAddTraits(.isButton)
        } else {
            row
        }
    }

    private var row: some View {
        HStack(spacing: 8) {
            (Text(model.verb).foregroundStyle(Palette.ink) + Text(" ") + Text(model.object).foregroundStyle(Palette.muted))
                .kithText(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 8)
            switch model.result {
            case nil:
                ProgressView().controlSize(.mini)
            case .text(let t):
                Text(t).kithText(.code).foregroundStyle(Palette.muted).lineLimit(1)
            case .error(let t):
                Text(t).kithText(.code).foregroundStyle(Palette.negative).lineLimit(1)
            }
        }
        .padding(.horizontal, 12)
        .frame(maxWidth: .infinity, minHeight: KithSize.toolRowHeight, alignment: .leading)
        .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.rowTool, style: .continuous))
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibility))
    }

    private var accessibility: String {
        let result: String = switch model.result {
        case nil: "running"
        case .text(let t): t
        case .error(let t): "error, \(t)"
        }
        return "\(model.verb) \(model.object), \(result)"
    }
}

/// The sheet's content: the command, then what it printed.
public struct ToolDetailSheet: View {
    let model: ToolCallRowModel

    public init(model: ToolCallRowModel) { self.model = model }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("\(model.verb) \(model.object)").kithText(.topBar).foregroundStyle(Palette.ink)
                if let command = model.command { block("Command", command) }
                if let output = model.output { block("Output", output) }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(KithSpacing.gutter)
        }
        .background(Palette.background)
    }

    private func block(_ title: String, _ text: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title.uppercased()).kithText(.label).foregroundStyle(Palette.muted)
            Text(text).kithText(.code).foregroundStyle(Palette.ink).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.rowTool, style: .continuous))
        }
    }
}

#Preview("Tool rows") {
    VStack(spacing: 8) {
        ToolCallRow(.init(verb: "Edited", object: "kith-sessions.py", result: .text("+6 -2"), command: "edit kith-sessions.py", output: "ok"))
        ToolCallRow(.init(verb: "Ran", object: "swift test --filter KithUITests"))
        ToolCallRow(.init(verb: "Ran", object: "cargo build --release", result: .error("failed")))
        ToolCallRow(.init(verb: "Read", object: "a/very/long/path/that/needs/to/truncate/in/the/middle/of/the/row.swift", result: .text("212 lines")))
    }
    .padding()
}

#Preview("Tool sheet") {
    ToolDetailSheet(model: .init(verb: "Ran", object: "swift test", result: .text("42"),
                                 command: "swift test --filter KithUITests", output: "Test run with 42 tests passed after 3.1 seconds."))
}
