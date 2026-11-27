import KithKit
import SwiftUI

/// What an agent is made of, read from its package: projects, harness, then skills, tools, MCP servers and plugins.
/// View only: changes go through builder (the agent page's Advanced page says so).
struct KithMadeOf: View {
    let info: KithInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            projects(info)
            harness(info)
            capabilities(info)
        }
    }

    @ViewBuilder func projects(_ info: KithInfo) -> some View {
        if info.errors["projects"] != nil {
            CardSection("Projects") { SheetNote(text: "Couldn't read the projects") }
        } else if !info.projects.isEmpty {
            CardSection("Projects") {
                ForEach(info.projects) { project in
                    SheetRow(icon: "briefcase", title: project.id,
                             detail: [project.files == 1 ? "1 file" : "\(project.files) files",
                                      project.modified.map { kithAgo($0.date) }].compactMap { $0 }.joined(separator: " · "))
                }
            }
        }
    }

    @ViewBuilder func harness(_ info: KithInfo) -> some View {
        // Model, effort and permission are in the bot's settings (hq-backed rows, once); the harness is what is left here.
        if let harness = info.harness {
            CardSection("Harness", footer: "Set in the agent's package") {
                SheetRow(icon: "cpu", title: harness)
            }
        }
    }

    @ViewBuilder func capabilities(_ info: KithInfo) -> some View {
        let errors = ["skills", "tools", "mcp_servers", "plugins"].contains { info.errors[$0] != nil }
        if errors || !(info.skills.isEmpty && info.tools.isEmpty && info.mcpServers.isEmpty && info.plugins.isEmpty) {
            CardSection("Skills, tools, plugins") {
                if info.errors["skills"] != nil { SheetNote(text: "Couldn't read skills") }
                else if !info.skills.isEmpty {
                    SheetCountRow(icon: "sparkles", title: "Skills", count: info.skills.count) {
                        ForEach(info.skills) { skill in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(skill.name).font(.system(size: 13, weight: .medium)).foregroundStyle(Palette.ink)
                                    if skill.source != "package" {
                                        Text(skill.source).font(.system(size: 10.5)).foregroundStyle(Palette.muted)
                                            .padding(.horizontal, 6).padding(.vertical, 1)
                                            .background(Palette.panel, in: Capsule())
                                    }
                                }
                                if !skill.description.isEmpty {
                                    Text(skill.description).font(.system(size: 11.5)).foregroundStyle(Palette.muted)
                                }
                            }
                        }
                    }
                }
                if info.errors["tools"] != nil { SheetNote(text: "Couldn't read tools") }
                else if !info.tools.isEmpty {
                    SheetCountRow(icon: "wrench.and.screwdriver", title: "Tools", count: info.tools.count) {
                        ForEach(info.tools, id: \.self) { Text($0).font(KithText.code.font).foregroundStyle(Palette.ink) }
                    }
                }
                if info.errors["mcp_servers"] != nil { SheetNote(text: "Couldn't read mcp servers") }
                else if !info.mcpServers.isEmpty {
                    SheetCountRow(icon: "server.rack", title: "MCP servers", count: info.mcpServers.count) {
                        ForEach(info.mcpServers) { server in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack(spacing: 6) {
                                    Text(server.name).font(.system(size: 13, weight: .medium)).foregroundStyle(Palette.ink)
                                    if let kind = server.kind { Text(kind).font(.system(size: 11.5)).foregroundStyle(Palette.muted) }
                                }
                                if let target = server.target {
                                    Text(target).font(KithText.code.font).foregroundStyle(Palette.muted).lineLimit(2)
                                }
                            }
                        }
                    }
                }
                if info.errors["plugins"] != nil { SheetNote(text: "Couldn't read plugins") }
                else if !info.plugins.isEmpty {
                    SheetCountRow(icon: "puzzlepiece.extension", title: "Plugins", count: info.plugins.count) {
                        ForEach(info.plugins, id: \.self) { Text($0).font(KithText.code.font).foregroundStyle(Palette.ink) }
                    }
                }
            }
        }
    }
}
