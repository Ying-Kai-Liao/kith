import KithKit
import SwiftUI

/// "New agent": every bot is a kith agent, so it needs a name and a line about what it does before it is made.
/// `like` fills the form from a flagged native row ("Make a kith agent like this"). A refusal from kith stays on the
/// sheet in the host's own words (a name taken).
public struct NewAgentSheet: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    @State private var form: NewAgent.Form
    @State private var running = false
    @State private var error: String?
    /// Made, but this app can't run its engine: nothing to open.
    @State private var madeOnly: String?

    public init(like bot: Bot? = nil) {
        _form = State(initialValue: bot.map(NewAgent.Form.init(like:)) ?? NewAgent.Form())
    }

    private var nameProblem: String? { form.name.isEmpty ? nil : NewAgent.nameError(form.name) }
    private var descriptionProblem: String? { form.description.isEmpty ? nil : NewAgent.descriptionError(form.description) }
    private var ready: Bool {
        NewAgent.nameError(form.name) == nil && NewAgent.descriptionError(form.description) == nil && model.connection == .online
    }

    public var body: some View {
        VStack(spacing: 0) {
            ModalHeader("New agent") {
                if running {
                    Spinner()
                } else if madeOnly == nil {
                    IconButton("Create", systemImage: "checkmark") { create() }.disabled(!ready)
                }
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    if let madeOnly {
                        Text(madeOnly).font(.footnote).foregroundStyle(Palette.secondary)
                    } else {
                        CardSection("Name") {
                            TextField("trip-planner", text: $form.name)
                                .plainTextInput()
                                .fieldBox()
                                .onChange(of: form.name) { error = nil }
                        }
                        Text(error ?? nameProblem ?? "Lowercase letters, numbers and dashes. It is how you open the agent from Terminal too.")
                            .font(.footnote).foregroundStyle(error != nil || nameProblem != nil ? Palette.danger : Palette.secondary)
                        CardSection("Description") {
                            TextField("Plans trips and books the boring parts", text: $form.description)
                                .fieldBox()
                        }
                        Text(descriptionProblem ?? "One line: what it does.")
                            .font(.footnote).foregroundStyle(descriptionProblem != nil ? Palette.danger : Palette.secondary)
                        CardSection("Optional") {
                            ValueRow("Engine") {
                                ChoicePicker(selection: $form.engine, options: NewAgent.engines.map { ($0.id, $0.label) })
                            }
                            TextField("Model (agent's default)", text: $form.model)
                                .plainTextInput()
                                .fieldBox()
                        }
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 8)
                .padding(.bottom, 24)
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .background(Palette.background)
    }

    private func create() {
        guard !running, ready else { return }
        running = true
        error = nil
        let name = form.name.trimmingCharacters(in: .whitespaces)
        Task {
            do {
                let made = try await model.newKithAgent(form)
                if let bot = made.bot {
                    model.selection = bot.id
                    dismiss()
                } else {
                    madeOnly = NewAgent.engineNotRunHere(made.agent)
                }
            } catch {
                let message = error.localizedDescription
                if NewAgent.madeButFailed(message, name: name), let bot = await model.waitForAgentBot(name) {
                    // The agent exists: open it.
                    model.selection = bot.id
                    dismiss()
                } else {
                    withAnimation(Motion.fade) { self.error = message }
                }
            }
            running = false
        }
    }
}
