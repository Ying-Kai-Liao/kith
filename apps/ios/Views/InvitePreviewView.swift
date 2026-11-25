import KithKit
import KithUI
import SwiftUI

/// The invite sheet's first step: who invited the person, to what, and what each part is good for.
/// Accepting continues into the sign-in and join; a preview that can't load never blocks that.
struct InvitePreviewView: View {
    let model: InvitePreviewModel
    let accept: () -> Void
    let dismiss: () -> Void

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: "envelope.open.fill")
                .font(.system(size: 44))
                .foregroundStyle(Palette.secondary)
                .padding(.top, 32)
            switch model.phase {
            case .loading:
                placeholder
            case .loaded(let preview):
                Text("\(preview.inviterName) invited you to \(preview.computerName)")
                    .font(.title3.weight(.semibold)).foregroundStyle(Palette.text).multilineTextAlignment(.center)
                if let note = preview.note, !note.isEmpty {
                    Text(note).font(.subheadline).foregroundStyle(Palette.secondary).multilineTextAlignment(.center)
                }
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(preview.items) { InvitePreviewRow(item: $0) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            case .generic:
                Text("You've been invited to a computer on Kith")
                    .font(.title3.weight(.semibold)).foregroundStyle(Palette.text).multilineTextAlignment(.center)
            }
            VStack(spacing: 12) {
                Button("Accept and sign in", action: accept).buttonStyle(.primary)
                Button("Not now", action: dismiss).buttonStyle(.secondary)
            }
            Spacer()
        }
        .padding(24)
        .frame(maxWidth: .infinity)
        .background(Palette.background)
        .task { await model.load() }
    }

    /// Grey bars where the title and rows will be.
    private var placeholder: some View {
        VStack(spacing: 12) {
            ForEach(0..<3, id: \.self) { _ in
                RoundedRectangle(cornerRadius: 6).fill(Palette.surface).frame(height: 18)
            }
        }
        .accessibilityLabel("Loading the invitation")
    }
}

/// One agent or room the person was invited to.
struct InvitePreviewRow: View {
    let item: InvitePreviewItem

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: item.kind == .agent ? "sparkles" : "person.2.fill")
                .frame(width: 28).foregroundStyle(Palette.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.name).font(.body.weight(.medium)).foregroundStyle(Palette.text)
                Text(item.suitableFor).font(.subheadline).foregroundStyle(Palette.secondary)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }
}
