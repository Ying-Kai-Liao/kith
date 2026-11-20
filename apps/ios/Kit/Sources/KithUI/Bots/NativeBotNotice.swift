import KithKit
import SwiftUI

/// What a flagged native row says in place of its composer and settings (one-bot-model C5): it no longer runs.
/// "Make a kith agent like this" opens New agent filled from the row; "Export" has the host write the row's settings,
/// memory and routines to a folder and shows where. Nothing is converted or deleted here.
struct NativeBotNotice: View {
    let bot: Bot
    @Environment(BotStore.self) private var model
    @Environment(AccountStore.self) private var accounts: AccountStore?
    @Environment(\.dismissModal) private var dismissDetails
    @State private var making = false
    @State private var exporting = false
    @State private var path: String?
    @State private var error: String?

    static let line = "This bot no longer runs. Make a kith agent like it, or export what it kept."

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(Self.line).font(.footnote).foregroundStyle(Palette.warning)
            if model.isOwner {
                HStack(spacing: 8) {
                    // One flow at a time: a second tap while the sheet is coming up does nothing.
                    Button("Make a kith agent like this") { making = true }
                        .disabled(making)
                    Button(exporting ? "Exporting…" : "Export", action: export)
                        .disabled(exporting)
                }
                .buttonStyle(.bordered)
            }
            if let path {
                Text("Exported to \(path) on \(model.hostName)").font(.footnote).foregroundStyle(Palette.secondary).textSelection(.enabled)
            }
            if let error { Text(error).font(.footnote).foregroundStyle(Palette.danger) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
        .kithSheet(isPresented: $making) { NewAgentSheet(like: bot) { made in open(made) } }
    }

    /// The agent is made: open its chat on this computer and close the details this notice may sit in.
    private func open(_ made: Bot) {
        if let accounts {
            accounts.selection = BotReference(accountId: accounts.selection?.accountId, computerId: model.computer.id, botId: made.id)
        }
        dismissDetails()
    }

    private func export() {
        guard !exporting else { return }
        exporting = true
        error = nil
        Task {
            do { path = try await model.exportHostBot(bot.id) } catch { self.error = error.localizedDescription }
            exporting = false
        }
    }
}
