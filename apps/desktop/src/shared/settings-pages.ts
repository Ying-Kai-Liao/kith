/** The Settings window's pages, in the sidebar's order (the same list as the iPhone's, then the Mac-only pages). */
export const SETTINGS_PAGES = ['account', 'crew', 'computers', 'people', 'keys', 'bots', 'voice', 'usage', 'notifications', 'privacy', 'updates', 'about'] as const
export type SettingsPage = (typeof SETTINGS_PAGES)[number]

export const isSettingsPage = (v: unknown): v is SettingsPage => typeof v === 'string' && (SETTINGS_PAGES as readonly string[]).includes(v)

/** `computers` is kept as a page name for the menu, the chat window and the recovery sheet; its content now lives in Account. */
export const resolveSettingsPage = (page: SettingsPage): SettingsPage => (page === 'computers' ? 'account' : page)
