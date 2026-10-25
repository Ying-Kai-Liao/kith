import KithKit
import SwiftUI

// The Access and Key cards (DESIGN.md "Reply cards"): an agent needs a service's key. The Access card hands it an
// account the owner already saved; the Key card takes a new key into a secure field that goes to the computer's
// Keychain and nowhere else. A key never enters the thread, a draft, the shared app-group store, a log or analytics:
// it lives in this view's @State from the first keystroke until Save succeeds or the card goes away.

/// What a card says once it is not pending, from the host's `secret.state` (and the entry's own status, which also
/// carries a card that expired or was cancelled).
enum SecretOutcome: Equatable {
    case pending, saved, denied, expired

    init(_ entry: Entry) {
        guard let request = entry.data.secret else { self = .pending; return }
        switch request.state {
        case "saved": self = .saved
        case "denied": self = .denied
        case "expired": self = .expired
        default:
            switch entry.data.status {
            case "expired", "cancelled": self = .expired
            default: self = .pending
            }
        }
    }

    func text(_ request: SecretRequest, account: Bool) -> String {
        switch self {
        case .pending: ""
        case .saved: account ? "Using \(request.profile ?? request.service)" : "Saved as \(request.profile ?? request.service)"
        case .denied: "Not now"
        case .expired: "Expired"
        }
    }
}

/// The card's frame and heading, shared by both cards.
private struct SecretFrame<Content: View>: View {
    let tint: AgentTint
    let heading: String
    let question: String
    var purpose: String?
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label {
                Text(heading).kithText(.label).foregroundStyle(Palette.muted)
            } icon: {
                Image(systemName: "key").font(.system(size: 12)).foregroundStyle(Palette.muted)
            }
            Text(question).kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.ink)
            if let purpose, !purpose.isEmpty {
                Text(purpose).kithText(.secondary).foregroundStyle(Palette.muted)
            }
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(tint.b.opacity(0.32), in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
        .accessibilityElement(children: .contain)
    }
}

/// A closed card: the outcome in the panel look the permission card uses once answered.
private struct SecretClosed: View {
    let heading: String
    let question: String
    let outcome: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(heading).kithText(.label).foregroundStyle(Palette.muted)
            Text(question).kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.ink)
            Text(outcome).kithText(.secondary).fontWeight(.medium).foregroundStyle(Palette.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
    }
}

private struct SecretButton: View {
    let title: String
    var primary = false
    var busy = false
    let action: () -> Void
    @Environment(\.isEnabled) private var enabled

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Text(title).kithText(.body).fontWeight(.semibold).lineLimit(1)
                if busy { ProgressView().controlSize(.mini) }
            }
            .foregroundStyle(primary ? Palette.background : Palette.ink)
            .frame(maxWidth: .infinity, minHeight: 44)
            .padding(.horizontal, 12)
            .background(primary ? Palette.ink : Palette.background.opacity(0.85),
                        in: RoundedRectangle(cornerRadius: KithRadius.rowTool + 4, style: .continuous))
            .opacity(enabled ? 1 : 0.4)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

private func serviceTitle(_ service: String) -> String {
    service.prefix(1).uppercased() + service.dropFirst()
}

// MARK: Access

/// "<agent> wants <Service>": the owner's accounts for the service as radio rows, "Another account", Allow / Not now.
/// The option ids are the host's: `profile:<name>`, `another`, `deny`.
struct AccessCard: View {
    let entry: Entry
    let agentName: String
    var tint: AgentTint = .neutral
    /// The option whose answer is on its way, if any.
    var answering: String?
    let respond: (String) -> Void
    @State private var choice: String?

    private var request: SecretRequest? { entry.data.secret }
    private var accounts: [SecretAccount] { entry.data.accounts ?? [] }
    private var selection: String? { choice ?? accounts.first.map { SecretRules.optionId(profile: $0.profile) } ?? "another" }

    var body: some View {
        if let request {
            let outcome = SecretOutcome(entry)
            let heading = "\(agentName) wants \(serviceTitle(request.service))"
            Group {
                if outcome == .pending {
                    SecretFrame(tint: tint, heading: heading, question: "Which \(serviceTitle(request.service)) account should \(agentName) use?",
                                purpose: request.purpose) {
                        VStack(spacing: 6) {
                            ForEach(accounts) { account in
                                row(id: SecretRules.optionId(profile: account.profile), title: account.account,
                                    detail: account.agents.isEmpty ? "Not used by an agent yet" : "Used by \(account.agents.joined(separator: ", "))")
                            }
                            row(id: "another", title: "Another account", detail: "Add a different key")
                        }
                        Text("Only \(agentName) · until you take it back").kithText(.secondary).foregroundStyle(Palette.muted)
                        HStack(spacing: 8) {
                            SecretButton(title: "Not now", busy: answering == "deny") { respond("deny") }
                            SecretButton(title: selection == "another" ? "Continue" : "Allow", primary: true,
                                         busy: answering != nil && answering != "deny") { if let s = selection { respond(s) } }
                        }
                        .disabled(answering != nil)
                    }
                } else {
                    SecretClosed(heading: heading, question: "\(serviceTitle(request.service)) account", outcome: outcome.text(request, account: true))
                }
            }
            .padding(.trailing, 40)
        }
    }

    private func row(id: String, title: String, detail: String) -> some View {
        let on = selection == id
        return Button { choice = id } label: {
            HStack(spacing: 10) {
                Image(systemName: on ? "largecircle.fill.circle" : "circle").foregroundStyle(on ? Palette.ink : Palette.muted)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).kithText(.body).foregroundStyle(Palette.ink)
                    Text(detail).kithText(.secondary).foregroundStyle(Palette.muted).lineLimit(2)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            .background(Palette.background.opacity(0.85), in: RoundedRectangle(cornerRadius: KithRadius.rowTool + 4, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(answering != nil)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

// MARK: Key

/// The Key card: an account label (unless it replaces a key), one secure field per variable, Save / Not now.
/// `save` sends the values and throws the host's error text; the card clears its fields on success, on leaving
/// the screen and when another device answers it.
struct KeyCard: View {
    let entry: Entry
    let agentName: String
    let hostName: String
    var tint: AgentTint = .neutral
    var answering: String?
    let save: (_ account: String?, _ values: [String: String]) async throws -> Void
    let deny: () -> Void

    // The key lives only here.
    @State private var account = ""
    @State private var values: [String: String] = [:]
    @State private var reveal = false
    @State private var sending = false
    @State private var error: String?
    @State private var justSaved = false
    @FocusState private var focus: String?

    private var request: SecretRequest? { entry.data.secret }

    var body: some View {
        if let request {
            let outcome = SecretOutcome(entry)
            let heading = "\(agentName) needs a key for \(serviceTitle(request.service))"
            Group {
                if outcome == .pending && !justSaved {
                    form(request, heading: heading)
                } else {
                    SecretClosed(heading: heading, question: "\(serviceTitle(request.service)) key",
                                 outcome: (justSaved && outcome == .pending ? SecretOutcome.saved : outcome).text(request, account: false))
                }
            }
            .padding(.trailing, 40)
            .onDisappear(perform: clear)
            // Answered on another device while typing: drop what was typed and show the state.
            .onChange(of: outcome) { _, now in if now != .pending { clear() } }
        }
    }

    private func form(_ request: SecretRequest, heading: String) -> some View {
        SecretFrame(tint: tint, heading: heading, question: "\(serviceTitle(request.service)) key for \(agentName)", purpose: request.purpose) {
            if request.replaces {
                Text("Replaces the key for \(request.profile ?? request.service)").kithText(.secondary).foregroundStyle(Palette.muted)
            } else {
                field("Account name", id: "account") {
                    TextField("", text: $account)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .focused($focus, equals: "account")
                }
                if !account.isEmpty, !SecretRules.validAccount(account) {
                    Text("Use lowercase letters, numbers, dots, dashes or underscores, up to 40.")
                        .kithText(.secondary).foregroundStyle(Palette.negative)
                }
            }
            ForEach(request.vars, id: \.self) { name in
                field(name, id: name) {
                    secure(name)
                }
            }
            HStack(spacing: 8) {
                Button { reveal.toggle() } label: {
                    Label(reveal ? "Hide" : "Show", systemImage: reveal ? "eye.slash" : "eye")
                        .kithText(.secondary).foregroundStyle(Palette.muted)
                }
                .buttonStyle(.plain)
                Spacer()
            }
            Text("Saved to the Keychain on \(hostName). No agent ever reads it; \(request.service)'s tool gets it when it runs.")
                .kithText(.secondary).foregroundStyle(Palette.muted)
            if let error {
                Text(error).kithText(.secondary).fontWeight(.medium).foregroundStyle(Palette.negative)
            }
            HStack(spacing: 8) {
                SecretButton(title: "Not now", busy: answering == "deny") { clear(); deny() }
                    .disabled(sending || answering != nil)
                SecretButton(title: "Save key", primary: true, busy: sending) { submit(request) }
                    .disabled(!canSave(request) || sending || answering != nil)
            }
        }
    }

    /// The secure field. `textContentType(nil)` keeps iOS from offering Passwords autofill or a save prompt;
    /// `privacySensitive` redacts it from the app switcher snapshot.
    @ViewBuilder private func secure(_ name: String) -> some View {
        let binding = Binding(get: { values[name] ?? "" }, set: { values[name] = $0 })
        Group {
            if reveal { TextField("", text: binding) } else { SecureField("", text: binding) }
        }
        .textContentType(nil)
        .autocorrectionDisabled()
        .textInputAutocapitalization(.never)
        .privacySensitive()
        .focused($focus, equals: name)
        .submitLabel(.done)
    }

    private func field<F: View>(_ title: String, id: String, @ViewBuilder _ input: () -> F) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).kithText(.label).foregroundStyle(Palette.muted)
            input()
                .kithText(.code)
                .padding(.horizontal, 12)
                .frame(minHeight: 44)
                .background(Palette.background, in: RoundedRectangle(cornerRadius: KithRadius.rowTool + 4, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: KithRadius.rowTool + 4, style: .continuous)
                    .stroke(Palette.ink, lineWidth: focus == id ? 1.5 : 0))
        }
    }

    private func canSave(_ request: SecretRequest) -> Bool {
        (request.replaces || SecretRules.validAccount(account))
            && !request.vars.isEmpty
            && request.vars.allSatisfy { SecretRules.validValue(trimmed(values[$0])) }
    }

    private func trimmed(_ value: String?) -> String { (value ?? "").trimmingCharacters(in: .whitespacesAndNewlines) }

    private func submit(_ request: SecretRequest) {
        guard !sending, canSave(request) else { return }
        sending = true
        error = nil
        focus = nil
        let sent = Dictionary(uniqueKeysWithValues: request.vars.map { ($0, trimmed(values[$0])) })
        let label = request.replaces ? nil : account
        Task {
            do {
                try await save(label, sent)
                clear()
                justSaved = true
            } catch {
                // Stay open with the fields as typed, so a retry is one tap.
                self.error = error.localizedDescription
            }
            sending = false
        }
    }

    private func clear() {
        values = [:]
        reveal = false
    }
}
