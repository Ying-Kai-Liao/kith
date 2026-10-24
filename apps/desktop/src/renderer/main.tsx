import { createRoot } from 'react-dom/client'
import './styles/theme.css'
import { ChatWindow } from './views/ChatWindow'
import { WebWindow } from './web/WebWindow'
import { PairingWindow } from './views/PairingWindow'
import { SettingsWindow } from './views/settings/SettingsView'
import { isSettingsPage } from '@shared/settings-pages'
import { IslandWindow } from './views/island/IslandWindow'
import { IncomingPanel } from './views/call/IncomingPanel'
import { IslandModel } from './views/island/island-model'
import { AppModel } from './store/app-model'
import { AppContext } from './store/context'
import { applyTextSize, prefs } from './lib/prefs'
import { account } from './store/account'
import { errorReport } from '@shared/error-report'
import { migrateLegacyStorage } from '@shared/legacy-compat'
import { parseAppearance } from '@shared/appearance'

// Error tracking: this window's uncaught errors go to the host (shared/error-report.ts).
const report = (error: unknown, mechanism: string) => window.kith.app.reportError(errorReport(error, 'desktop-renderer', mechanism))
// An error event without an error (the ResizeObserver loop notice) has nothing to report.
window.addEventListener('error', (e) => { if (e.error != null) report(e.error, 'onerror') })
window.addEventListener('unhandledrejection', (e) => report(e.reason, 'unhandledrejection'))

migrateLegacyStorage(localStorage)
// The browser build is one member-only window (src/renderer/web); `?window=` selects nothing there.
const page = window.kith.platform === 'web' ? 'web' : (new URLSearchParams(location.search).get('window') ?? 'chat')
document.title = window.kith.appName
document.documentElement.dataset.platform = window.kith.platform
document.documentElement.dataset.window = page
applyTextSize(prefs.textSize.get())
// The main process applies the choice to every window and keeps its own copy for the next launch.
const sendAppearance = () => window.kith.app.setAppearance(parseAppearance(prefs.appearance.get()))
sendAppearance()
prefs.appearance.subscribe(sendAppearance)
const root = createRoot(document.getElementById('root')!)

if (page === 'island') {
  // The island only draws what the chat window sends it: no host connection of its own.
  const island = new IslandModel()
  island.start()
  root.render(<IslandWindow model={island} />)
} else if (page === 'incoming-call') {
  // The floating call panel only draws the call the main process hands it: no host connection of its own.
  root.render(<IncomingPanel />)
} else {
  // The pairing window only needs this computer's host, not a mirror of its bots.
  const app = new AppModel(page !== 'pairing')
  void app.start()
  if (page !== 'pairing') void account.start()
  const first = new URLSearchParams(location.search).get('page')
  root.render(
    <AppContext.Provider value={app}>
      {page === 'web' ? <WebWindow /> : page === 'pairing' ? <PairingWindow /> : page === 'settings' ? <SettingsWindow initial={isSettingsPage(first) ? first : 'account'} /> : <ChatWindow />}
    </AppContext.Provider>,
  )
}
