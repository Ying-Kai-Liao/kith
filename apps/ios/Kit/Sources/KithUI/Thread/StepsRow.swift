import KithKit
import SwiftUI

/// A run of tool calls as one quiet line: "3 steps" with a chevron, or "Working · 3 steps" while one still runs.
/// Tap opens the steps inline; no tool names or commands show until then.
struct StepsRow: View {
    let run: ThreadFeed.StepsRun
    let expanded: Bool
    let toggle: () -> Void
    /// "N earlier steps" past the cap: the full record.
    let openTrace: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: toggle) { label }
                .buttonStyle(.plain)
                .accessibilityLabel(Text(Self.title(run)))
                .accessibilityHint(Text(expanded ? "Hides the steps" : "Shows the steps"))
            if expanded {
                if run.hidden > 0 {
                    Button(action: openTrace) {
                        Text(run.hidden == 1 ? "1 earlier step" : "\(run.hidden) earlier steps")
                            .kithText(.secondary).foregroundStyle(Palette.muted).frame(minHeight: 32, alignment: .leading)
                    }
                    .buttonStyle(.plain)
                }
                ForEach(run.shown) { entry in
                    ToolCallRow(ThreadFeed.toolRow(entry)).padding(.top, 4)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var label: some View {
        HStack(spacing: 6) {
            if run.isRunning { ProgressView().controlSize(.mini) }
            (Text(Self.count(run, working: run.isRunning)).foregroundStyle(Palette.muted)
                + Text(run.failed > 0 ? " · \(run.failed) failed" : "").foregroundStyle(Palette.negative))
                .kithText(.secondary)
            Image(systemName: "chevron.right")
                .font(.system(size: 10, weight: .semibold)).foregroundStyle(Palette.muted)
                .rotationEffect(.degrees(expanded ? 90 : 0))
        }
        .frame(minHeight: 32, alignment: .leading)
        .contentShape(Rectangle())
    }

    /// "1 step" / "3 steps"; "Working · 3 steps" while one runs.
    static func count(_ run: ThreadFeed.StepsRun, working: Bool) -> String {
        let n = run.steps.count == 1 ? "1 step" : "\(run.steps.count) steps"
        return working ? "Working · \(n)" : n
    }

    static func title(_ run: ThreadFeed.StepsRun) -> String {
        count(run, working: run.isRunning) + (run.failed > 0 ? ", \(run.failed) failed" : "")
    }
}
