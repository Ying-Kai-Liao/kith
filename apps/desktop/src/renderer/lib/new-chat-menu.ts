import type { MenuItem } from '../components/Overlay.tsx'

/** The new-chat button's menu, in order. Agents are "agents" in every word of it. */
export function newChatMenu(go: { agent: () => void; person: () => void; bot: () => void; group: () => void }): MenuItem[] {
  return [
    { title: 'New agent', icon: 'square.and.pencil', action: go.agent },
    { title: 'Invite a person', icon: 'person.crop.circle.badge.plus', action: go.person },
    { title: 'Invite a bot', icon: 'link', action: go.bot },
    { title: 'New group chat', icon: 'person.2', divider: true, action: go.group },
  ]
}
