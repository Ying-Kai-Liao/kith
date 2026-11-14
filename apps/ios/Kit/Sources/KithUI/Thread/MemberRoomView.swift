import KithKit
import SwiftUI

/// An invited person's room (SPEC §8-§10): its info with Leave, the one neutral closed state, the two opening lines and
/// "Asked <agent>". The room itself is the ordinary group chat; these are the parts only a person sees.

/// Keeps an open room fresh: the host pushes nothing to people, so it is read on open, on foreground and every few seconds.
private struct MemberRoomRefresh: ViewModifier {
    let botId: String
    @Environment(BotStore.self) private var model
    @Environment(\.scenePhase) private var scenePhase

    func body(content: Content) -> some View {
        content.task(id: scenePhase) {
            guard model.myRooms[botId] != nil, scenePhase == .active else { return }
            while !Task.isCancelled {
                // The list first: a new stay drops the old messages before the room is read again.
                await model.refreshMyRooms()
                await model.loadRoom(botId)
                try? await Task.sleep(for: .seconds(8))
            }
        }
    }
}

extension View {
    func memberRoomRefresh(_ botId: String) -> some View { modifier(MemberRoomRefresh(botId: botId)) }
}

/// Room info for a person: the room's agents, whose room it is, and Leave. No people list (their app has no source for it).
struct MemberRoomInfoView: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    let roomId: String
    @State private var leaving = false

    private var room: HostContract.MyRoom? { model.myRooms[roomId] }
    private var name: String { room?.name ?? model.bots[roomId]?.name ?? "Room" }

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader(name) {}
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text("AGENTS").font(.system(size: 13, weight: .semibold)).tracking(0.8).foregroundStyle(Palette.muted).padding(.leading, 4)
                    VStack(spacing: 0) {
                        ForEach(room?.agents ?? [], id: \.self) { agent in
                            HStack {
                                Text(agent).kithText(.agentName).foregroundStyle(Palette.ink)
                                Spacer()
                                Text("Answers when @'d").kithText(.secondary).foregroundStyle(Palette.muted)
                            }
                            .padding(.horizontal, 14).frame(minHeight: 56)
                            if agent != room?.agents.last { Divider().padding(.leading, 14) }
                        }
                    }
                    .overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
                    Text(MemberRoomLogic.ownerLine(room: name)).kithText(.secondary).foregroundStyle(Palette.muted).padding(.horizontal, 4)
                    Button { leaving = true } label: {
                        Text("Leave \(name)").font(.system(size: 17, weight: .semibold)).foregroundStyle(Palette.danger)
                            .frame(maxWidth: .infinity, minHeight: 48)
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 20).padding(.bottom, 24)
            }
        }
        .background(Palette.background)
        .kithDialog("Leave \(name)?", isPresented: $leaving, message: "You stop seeing \(name). Kai can add you again.") {
            [DialogAction("Leave", destructive: true) {
                Task { await model.leaveRoom(roomId); dismiss() }
            }]
        }
    }
}

/// One neutral state for a room that is gone: left on another device, removed, or closed. The app can't tell, and doesn't guess.
struct MemberRoomClosed: View {
    let name: String
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(spacing: 14) {
            Text(MemberRoomLogic.closedLine(room: name)).kithText(.body).foregroundStyle(Palette.ink)
            Button { model.selection = nil } label: {
                Text("Back").font(.system(size: 17, weight: .semibold)).foregroundStyle(Palette.ink)
                    .padding(.horizontal, 28).frame(minHeight: 48)
                    .overlay(Capsule().strokeBorder(Palette.border, lineWidth: 1))
            }
            .buttonStyle(.plain)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Palette.background)
    }
}

/// The two opening lines at the top of a person's history, once no older page is left.
struct MemberStartLines: View {
    let room: String

    var body: some View {
        VStack(spacing: 2) {
            ForEach(MemberRoomLogic.startLines(room: room), id: \.self) { Text($0).font(.caption).foregroundStyle(Palette.muted) }
        }
        .frame(maxWidth: .infinity).padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }
}

/// "Asked planner" under the person's newest message that @'d an agent, until an agent reply lands.
struct AskedLine: View {
    let text: String

    var body: some View {
        Text(text).font(.caption).foregroundStyle(Palette.muted).frame(maxWidth: .infinity, alignment: .trailing).padding(.trailing, 16)
    }
}
