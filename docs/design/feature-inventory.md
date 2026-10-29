# Kith redesign: feature inventory and build order

Status: draft, 2026-10-23. Written before any screen is rebuilt.

**Hard rule (Kai, 2026-10-23): do not break or reduce features. We can redo them very similarly.** Nothing in this file is marked "remove". If the design has no obvious home for a feature it is marked `OPEN: needs a home` and listed in [Open](#open-features-without-a-home).

The design is [`DESIGN.md`](DESIGN.md) (owned by the designer; mocks in [`mocks/`](mocks/)). It is written for iOS 26 ("stock iOS underneath, a thin Kith layer on top"). It says nothing specific about the desktop app, so the desktop rows assume the same layout (Crew / Board, chat box dock, agent sheet) adapted to a window; see Open item 12.

Verdicts:
- **keep**: same feature and place in the flow, restyled with the new tokens.
- **adapt**: same feature, moves or changes shape inside the new layout.
- **replace (with X)**: the old control goes away, X does its job. The feature itself stays.
- **OPEN**: no home in DESIGN.md yet.

"New home" names a screen or component of DESIGN.md: *Crew home* (list of rooms + top bar + dock), *Board*, *Agent chat* (threads feed), *Agent sheet* (Details / Memory / Settings), *Profile menu* (account, Computers, Settings), *Settings*, *Chat box* (dock), *Call*, and the eight components (bot avatar, crew row, handoff card, decision card, transcript turn, tool row, status chips, composer/chat box) plus place chip, colour sheet, correct.

Row IDs (`I-A3`, `D-71`) are what the build order below refers to. Code paths were read, not guessed; the desktop main-process rows come from reading the code, not from running the app.

Things that exist in the apps but not in DESIGN.md at all, found while inventorying: the State tab (widget / Live Activity / Dynamic Island setup), Extensions (marketplace) and Credentials, the remote Computer screen, the call screen's own look, first-run welcome and pairing, the desktop notch island, the star prompt, message reactions, bot-to-bot conversations. They are all in the Open section.


## iOS app

Paths are relative to `apps/ios/`. `KUI` = `Kit/Sources/KithUI`, `KK` = `Kit/Sources/KithKit`. The app today is a `TabView` (Bots | State) over a `NavigationStack`, with a system navigation bar and a "Bots" inline title; the design replaces that shell, so every row in "Shell and lists" changes shape but none may disappear.

### I-A. Shell, roster, top bar

| ID | Screen / feature | Code | What it does today | New home in DESIGN.md | Verdict |
|---|---|---|---|---|---|
| I-A1 | Tab bar: Bots, State | `Views/RootView.swift` (`tabs`) | Two system tabs; both stay alive; bar hides while a chat is open; opening a bot from a notification, widget or link lands on Bots | Removed by "No tab bar". Bots tab becomes Crew; Crew \| Board switch replaces tabs | replace (Crew \| Board glass switch) |
| I-A2 | State tab: Widget / Live Activity / Dynamic Island galleries (see I-J) | `Views/RootView.swift` (`StateTab`) | Gallery to design and switch on each surface outside the app | None in DESIGN.md (no tab bar) | OPEN: needs a home (proposal: Profile menu > Settings > "Widgets & Live Activities") |
| I-A3 | Bots roster list | `Views/BotListView.swift` | One row per bot across all computers, grouped by computer heading when more than one; pull to refresh restarts every computer stream and refreshes the account | Crew list (one row per room, sorted needs-you, working, recent) | adapt (rooms, not machines; keep refresh, keep reorder if the design allows) |
| I-A4 | Computer headings in roster (badge, host name, status text, route icon, collapse chevron, "No bots yet") | `Views/BotListView.swift` (`ComputerSection`) | Tap folds a computer's bots; drag moves the computer (order kept on device) | "One crew, not machines"; machines only when they matter | adapt (machine trouble shows as one status chip; computer order and folding move to Profile menu > Computers) |
| I-A5 | Bot row: avatar with status, pin glyph, name, "using the computer" glyph, relative time, one-line preview (needs approval / activity / error / last message / harness), unread count capsule | `KUI/Bots/BotRow.swift` | Roster row | Crew row (bot 48, Nunito name, time or state, one sentence; ink count badge for needs-you) | adapt (keep every state: needs you, working, error, last message, unread, pinned, using-the-computer) |
| I-A6 | Group avatar in rows (member faces) | `KK/Design/CrewAvatar.swift`, `KUI/Chrome.swift` (`AvatarWithStatus`, `GroupAvatar`) | Group shown as stacked member faces | Group row = two bots on the diagonal + one sky patch | adapt |
| I-A7 | Row long-press menu: Pin/Unpin, Edit profile / Edit group, Mark as read, Hide from list, Delete (with confirm dialog; group wording differs) | `Views/BotListView.swift` (`contextActions`, `kithDialog`) | System-style lift menu | Crew long-press: pin, colour, sleep, share | adapt (design lists pin, colour, sleep, share; keep Edit, Mark as read, Hide, Delete in the same menu) |
| I-A8 | Drag to reorder bots (synced to host) and accessibility Move up / Move down | `Views/BotListView.swift` (`moveLines`), `KUI/Store/BotStore+Actions.swift` | Reorder roster | Crew list sort rule is needs-you, working, recent | OPEN: needs a home (manual order vs. the design's fixed sort; pinned rows keep a place) |
| I-A9 | Hidden bots list in Settings with Unhide | `Views/SettingsView.swift` ("Hidden bots") | Restores bots hidden from the roster | Settings (Profile menu) | keep |
| I-A10 | Empty roster: "No bots yet", cloud-avatar art, "Create your first bot" or "Computers" button; copy differs when no computer is paired | `Views/BotListView.swift` (`EmptyRoster`) | First-run roster | Crew empty state | adapt |
| I-A11 | Profile / account button (top left) | `Views/AccountSwitcherView.swift` (`AccountSwitcherButton`) | Opens the Account sheet | Profile picture (piece of sky) with menu: account, Computers, Settings | adapt |
| I-A12 | Computer filter header (centre title): show all / show only one computer, Reconnect, Manage computers; summary text | `KUI/Bots/ComputerFilterHeader.swift` | Filters roster by computer on this device | No machine title; Crew \| Board switch in the centre | replace (filter moves under Profile > Computers; Reconnect stays there) |
| I-A13 | New menu (plus): New bot, New group chat (disabled when nothing online or all computers need update) | `Views/BotListView.swift` | Opens bot editor or group editor | New-chat menu: New bot (template or crew), Invite a person, New group chat | adapt (add Invite a person only when backend supports it; keep the disabled rules) |
| I-A14 | Computer update-needed card at top of roster (per computer) | `KUI/UpdateNeededCard.swift`, `Views/BotListView.swift` | "Update this app" / "Update Kith on <host>" with instructions; blocks new bots | Status chips: "Mac unreachable", machine trouble as one chip, one tap from Computers | adapt (chip in the list leading to a card in Computers; the full text must stay) |
| I-A15 | Access banners (bottom): "Approve on <computer>", 6-digit code, cancel | `Views/RootView.swift` (`AccessBanners`) | Shows while this iPhone waits for a computer to approve | Status chip above the dock; detail in Computers | adapt |
| I-A16 | Global error dialog "Something went wrong" | `Views/RootView.swift` | One dialog for any account or computer error | stock alert | keep |
| I-A17 | Chat opened from a removed computer: "This computer was removed" | `Views/RootView.swift` (`ChatScreen`) | Empty state with back | Agent chat error state | keep |
| I-A18 | Launch splash "Connecting" | `Views/LaunchSplash.swift` | Cold-start animation | Crew home loading state (sky + bots) | adapt |
| I-A19 | Unreachable / offline / signed-out states of the list (status text per computer, warning colour, "Offline") | `KUI/Store/AccountStore.swift` (`statusText`), `Views/ComputerStatus.swift` | Per-computer status in headings | Status chips: Live, Catching up, Mac unreachable | adapt |

### I-B. Chat box (composer), search, routing

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-B1 | Composer: multi-line field, placeholder per mode ("Ask <name>", "Queue a message for <name>" while working, "Reply…" in thread, "Message everyone · @ to ask one bot" in group, "New task for <name>"), send, stop | `KUI/Thread/Composer.swift` | Bottom of a chat | Chat box in the dock ("Tell coder…") | adapt (keep every placeholder mode and the queue-while-working behaviour) |
| I-B2 | Attach menu (+): Photos, Files, Paste, New task | `KUI/Thread/Composer.swift`, `ComposerAttachment.swift`, `Attachments.swift` | Adds files and images; drop of files onto the box works | + inside the chat box (Photos, Camera, Files) | adapt (add Camera; "New task" becomes redundant because every message is a new thread, but keep until behaviour is confirmed) |
| I-B3 | "New task" chip over the box with starting folder and x | `KUI/Thread/Composer.swift` (`newTaskChip`) | Next message starts a task, optionally in a chosen folder | Place chip + "Every message starts a new thread" | replace (place chip on the route card) |
| I-B4 | Dictation (mic) | `KUI/Thread/Dictation.swift`, `Composer.swift` | Speech to text into the draft | Mic in the chat box (empty state) | keep |
| I-B5 | Slash commands list (`/fast`, installed skills as `/skill`) | `KUI/Thread/SlashCommands.swift`, `CommandList.swift` | `/` opens commands, Fast mode toggle, skill commands | `/` opens commands | keep |
| I-B6 | @mention suggestions in group chats | `KUI/Thread/Composer.swift` (`mentionQuery`) | Pick a member to ask just that bot | `@name` skips routing | keep |
| I-B7 | Queued messages while offline: "Sending…", "Not sent", "Waiting for the computer to come online" with cancel, "Delivered to the computer", "Failed to send" with Resend / Delete, Unsend | `KUI/Thread/ChatRows.swift` (`UserBubble`) | Delivery states under your bubble | Bubble delivery states (stay as they are) | keep |
| I-B8 | Home search / router ("Find or ask…", results in place, "Goes to: coder · new session", "Search the crew's memory", "Ask the crew") | none today | not built | Chat box (home) | OPEN: new feature, not in the app yet. Build after the dock exists; see Open items |
| I-B9 | Call button next to the box, hides while typing | none on home today (call is in chat header and agent sheet) | not built | Round call button right of the chat box (Crew and Board) | adapt (new placement; calls the agent the box would route to, or picks one) |

### I-C. Agent chat

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-C1 | Chat top bar: back, bot avatar + name, connection subtitle (host + route: Connecting / Offline / No access / Not paired / Needs update / Connected via Wi-Fi or Cloudflare), call button, computer button | `KUI/Thread/ThreadView.swift` (`chrome`, `header`, `callButton`, `computerButton`) | System nav bar | Agent chat top: back, bot + name, phone + ••• pair; header picture + name opens the agent sheet | adapt (connection text becomes a status chip; Computer button needs a home, see Open) |
| I-C2 | Header menu: host + connection line, Reconnect, Details, Full conversation, Edit group, Settings, Pin/Unpin, Routines, New session (confirm), Delete bot / Delete group (confirm) | `KUI/Thread/ThreadView.swift` (`menuItems`) | Title-pill menu | ••• menu in agent chat and agent sheet | adapt (add "Correct…" per design; keep every item) |
| I-C3 | Place chip under the bar (folder, project) and "Where coder works" picker: Recent, hq projects, Choose a folder, register project, confirm "Switch folder?" | `KUI/Bots/PlaceFolderPicker.swift`, `FolderPicker.swift`, `KK/Models/PlaceFolder.swift`, `KUI/Store/BotStore+Folders.swift` | Chooses where the thread runs | Place chip + "Where coder works" sheet (Suggested, Projects, Choose a folder, Mac) | adapt (add Suggested with reasons; keep register-a-project flow and hidden-folder toggle) |
| I-C4 | Transcript: time separators, user bubble (right), agent bubble (left, markdown), author labels in groups, "answered by backup" label, notices (call ended with duration), working indicator | `KUI/Thread/ChatRows.swift`, `MarkdownText.swift`, `MarkdownBlocks.swift`, `AnsweredByLabel.swift`, `ConversationList.swift` | Chat feed | Transcript turn + working indicator | keep (restyle) |
| I-C5 | Task row (folded task with status icon, title, "N replies · day", stop, continue, fold) | `KUI/Thread/TaskRow.swift`, `KK/Models/Tasks.swift` | The previous design's thread chip | Thread chip: bot avatar, "N replies", "1 new", time, place beside, down-arrow; newest open, idle folded | adapt (this is the closest existing match; keep Stop this task and Continue) |
| I-C6 | Reply in thread / thread sheet ("Thread", close) | `KUI/Thread/RepliesView.swift`, `ThreadView.swift` (`openThread`) | Full-screen thread to continue | Tapping chip text opens the thread full screen | keep |
| I-C7 | Bubble menus: user (Copy, Reply in thread, Unsend, emoji reactions), agent (Copy, Reply in thread, Show what it did) | `KUI/Thread/ChatRows.swift`, `KUI/Controls.swift` (`contextActions`) | Long-press | Long-press agent reply adds "Correct this" | adapt (add Correct this; keep reactions) |
| I-C8 | Reactions row | `KUI/Thread/ChatRows.swift` (`ReactionsRow`) | Emoji reactions on messages | none (design says SF Symbols, never emoji for icons; reactions are user content) | OPEN: needs a home (reactions are user-chosen emoji; confirm with the designer) |
| I-C9 | Permission card (Allow / Deny / Always, command, diff, "Runs on host · folder", Details) | `KUI/Thread/PermissionCard.swift` | Approval prompt inline | Decision card (permission prompts use the same card) | adapt |
| I-C10 | Decision / question buttons inline in feed (agent asks) | `KUI/Thread/ChatRows.swift`, `PermissionCard.swift` | Answer chips under agent message | Decision card, tinted with the agent's colours, answerable without opening the thread; ink count badge on the Crew row | adapt |
| I-C11 | Tool rows and Trace sheet "Full conversation" (thinking, plan, approvals, tool calls with result) | `KUI/Thread/TraceView.swift` | Sheet with all steps | Tool row (44 pt, verb + object, result right, tap = stock sheet with command and output); no terminal view | adapt |
| I-C12 | Handoff card ("What it carries", Keep this conversation, edit) and handoff editor | `KUI/Thread/HandoffRows.swift` | Hand over to a fresh session | Handoff card ("Where we left off") | adapt (keep Keep and Edit; design wants only real fields) |
| I-C13 | File download card, export sheet | `KUI/Thread/FileDownloadCard.swift`, `KUI/Store/FileDownloads.swift` | Files the agent shares; download, cancel, share sheet | none specified | keep (restyle as a bubble attachment) |
| I-C14 | Attachment list in bubbles with image viewer | `KUI/Thread/Attachments.swift` | Shows sent and received images/files | none specified | keep |
| I-C15 | Bot-to-bot conversation sheet ("<A> and <B>", waiting for…) | `KUI/Thread/BotConversation.swift` | Peeks into bots talking to each other inside a group | none specified | OPEN: needs a home (group modes beyond DESIGN.md) |
| I-C16 | Intro cards: bot intro (avatar, description, hint), group intro ("Everyone answers in turn. @mention a bot…"), earlier messages loading, pull to load earlier | `KUI/Thread/ThreadView.swift` (`IntroCard`, `GroupIntro`), `PullToRefresh.swift` | Empty-chat state | Agent chat empty state | keep |
| I-C17 | Routine run entries in feed (clock glyph) | `KUI/Thread/ThreadView.swift` | Shows routine runs | Thread chips for routine-started threads | adapt |
| I-C18 | New session (confirm "Start a new session?") | `KUI/Thread/ThreadView.swift` | Fresh context, history kept | Every message is a new session, old folds | adapt (keep as an explicit action in ••• menu until sessions are implicit) |
| I-C19 | Update-needed card replaces the composer | `KUI/Thread/ThreadView.swift` | Blocks sending when host and app mismatch | Status chip in the dock | adapt |
| I-C20 | Call disabled reasons: "This chat is no longer available.", "<host> is offline.", "Update Kith to make calls." | `KUI/Thread/ThreadView.swift` (`callBlock`) | Phone button disabled with reason | Round call button with reason | keep |
| I-C21 | Correct this / proposed-fix card | none today | not built | Correct (context3.png B) | OPEN: new feature; not built until the Memory/correction backend is confirmed |

### I-D. Agent sheet: Details / Memory / Settings

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-D1 | Details sheet with Details / Memory / Settings tabs, close, "Create template", large avatar, name, backend and model, kith identity line, Call capsule + blocked reason | `KUI/Bots/BotDetailsView.swift` | Already three tabs, autosaves 600 ms after the last change | Agent sheet (Details \| Memory \| Settings) | keep (restyle; add small "Correct" beside Call) |
| I-D2 | Details tab: kith sections About, Cost (today/7d/30d/all, requests in 30 days, per model), Schedules, Projects, Harness, Skills / Tools / Plugins groups, instructions row, package chip menu (dirty, unpushed, pinned, version), pull to refresh, Retry, errors | `KUI/Bots/KithDetailsSections.swift`, `KithInfoStore.swift` | Read-only agent facts from kith | Agent sheet > Details | keep |
| I-D3 | Details tab: Instructions, Agent picker (backend, custom ACP command, install hint), model picker (model list, Fast mode, refresh/retry, free-text model id), hq settings rows | `KUI/Bots/BotEditorView.swift`, `AgentModelPicker.swift`, `HqSettingsRows.swift` | Often-changed settings | Agent sheet > Details/Settings | keep |
| I-D4 | Details tab: "Computer" switch "Use the computer" with setup hint | `KUI/Bots/BotDetailsView.swift` | Lets the bot use the screen | Agent sheet > Settings | keep |
| I-D5 | Details tab: Routines section (list, add, ask the bot for a routine) | `KUI/Routines/RoutinesView.swift` | Per-bot routines | Agent sheet > Details (Routines card) | keep |
| I-D6 | Settings tab: Profile (avatar picker, name), Workspace (computer, folder: Personal workspace / Choose project folder, default hq project, permissions), Activity (notifications switch, use the computer), plugin toggles with info | `KUI/Bots/BotEditorView.swift`, `PluginToggles.swift`, `FolderPicker.swift` | Rarely-changed settings | Agent sheet > Settings | keep |
| I-D7 | Avatar and colour picker | `KUI/Bots/BotEditorView.swift` (`AvatarPicker`), `KK/Design/LivingBotTint.swift` | Choose body and colour | Colour sheet (preview, Gradient \| Plain, preset swatches, Reset to default) | adapt |
| I-D8 | Memory tab: hq memory card (files, history, versions, restore/undo, tidy runs with Undo, diff, edit file with conflict dialogs) | `KUI/Bots/HqMemoryView.swift`, `KithMemorySection.swift`, `KithMemoryFileView.swift`, `KUI/Store/HqMemoryStore.swift` | Memory for hq-backed agents | Agent sheet > Memory; history entries "You · corrected" with Undo | keep |
| I-D9 | Memory tab for plain bots: shared memory ("Use what your bots share"), own memory switch, entries with Edit / Pin / Archive / Forget, More menu (Refresh, Forget everything), add, tidy runs with Undo, harness memory (read-only), notes files (new note, edit, delete, versions, bring back) | `KUI/Bots/MemoryView.swift`, `MemoryFilesView.swift`, `MemoryEditorView.swift`, `HarnessMemorySheet.swift`, `KUI/Store/MemoryStore.swift` | Per-bot and shared memory | Agent sheet > Memory | keep |
| I-D10 | Create template sheet | `KUI/Bots/BotTemplateView.swift` | Copy settings to reuse | New bot (template or crew) | keep |
| I-D11 | Bot editor (new bot): all sections, computer picker for new bots (offline marked), Create | `KUI/Bots/BotEditorView.swift` | Create flow | New bot from new-chat menu | keep |
| I-D12 | Group editor: name, members add/remove, Delete group chat | `KUI/Bots/GroupEditorView.swift` | Create and edit groups | New group chat | keep |

### I-E. Computers, account, settings

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-E1 | Account sheet: profile (photo, email), account switcher (add account), "Scan a computer's code", Computers row, Sign out (confirm), error line, signing-in orb | `Views/AccountSwitcherView.swift` | Opened by the top-left button | Profile menu: account, Computers, Settings | adapt |
| I-E2 | Sign-in buttons: Apple, Google, GitHub; "QR pairing only in this build" | `Views/SignInButtons.swift`, `AccountSwitcherView.swift`, `App/AccountSession.swift` | Sign-in | Profile menu / Welcome | keep |
| I-E3 | Settings "Computers & settings" page, section Computers: row per computer (badge, host, bot count, route, Extensions, Screen, route menu, in-account mark) with long-press menu (Color, Extensions, Voice chat, Screen, Revoke this iPhone's access, Remove) | `Views/SettingsView.swift` (`ComputerRow`) | Machine management | Profile menu > Computers (each Mac and its health) | keep |
| I-E4 | Account-only computers: "Ask for access", "Code", "Revoke", older-copy row with remove; access request flow (code check, waiting, approved, Try again / Ask again) | `Views/SettingsView.swift`, `Views/AccessRequestView.swift` | Cloud computers this iPhone may use | Computers | keep |
| I-E5 | Pair a computer (QR scan, pasted `kith://pair` link, Tailscale hint, Wi-Fi hint, camera unavailable) | `Views/PairingView.swift`, `KK/Client/Pairing.swift` | Pairing | Computers > Pair a computer; first-run setup | keep |
| I-E6 | Computer colour swatch panel | `Views/SettingsView.swift` (`SwatchPanel`) | Colour per computer | Computers | keep |
| I-E7 | Voice chat settings per computer: engine (On device, Kith voice, OpenAI, Gemini), voice choice, download voice status, API keys (save, test, replace, remove) | `KUI/Call/VoiceChatSettingsView.swift` | Voice settings | Settings > Voice | keep |
| I-E8 | Link Telegram or Discord (`/link` code with expiry, copy); "Schedule changes there too" switch (`scheduleMessages`, per person, on by default, hidden on older hosts) | `Views/SettingsView.swift` (`LinkChatSection`, `ScheduleMessagesToggle`) | Link chat accounts | Settings | keep |
| I-E9 | Backups status (per computer list of backup agents, fix hints) | `KUI/Bots/BackupsStatusSection.swift` | Shows backup chain | Settings or Computers | keep |
| I-E10 | Push notifications switch + OS permission row (On / Off in Settings / Turn on) | `Views/SettingsView.swift`, `App/Push.swift` | Notification control | Settings | keep |
| I-E11 | Text size (Default, Small, Large, Extra large) | `Views/SettingsView.swift` | Overrides Dynamic Type inside Kith | Settings (Dynamic Type applies) | keep |
| I-E12 | Share usage analytics switch + about sheet | `Views/SettingsView.swift`, `KUI/AnalyticsSheet.swift` | Privacy toggle | Settings | keep |
| I-E13 | App version, Source code link, Open-source licenses sheet | `Views/SettingsView.swift`, `KUI/LicensesSheet.swift` | About rows | Settings | keep |
| I-E14 | Start over (confirm) | `Views/SettingsView.swift` | Resets the app's local state | Settings | keep |
| I-E15 | Welcome: "Your coding agents, as teammates.", Get started, Watch the demo, sample chat | `Views/WelcomeView.swift`, `WidgetSetupDemo.swift` | First launch | First-run (not in DESIGN.md) | OPEN: needs a home (restyle with the sky and crew; keep copy and demo) |
| I-E16 | Pairing onboarding: Install Kith on your computer (copy command), Continue, Skip | `Views/PairingView.swift` | First launch step 2 | First-run | OPEN: needs a home (same as I-E15) |
| I-E17 | Computer's status text and route icon (Wi-Fi/Tailscale, Cloudflare) | `Views/ComputerStatus.swift` | Route indicator | Computers | keep |

### I-F. Marketplace (Extensions) and credentials

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-F1 | Extensions sheet: search; Agents (horizontal cards, load more, Node notice); Connectors (grid, skeleton, load more); Composio apps; Skills; Credentials; Make your own (custom connector, your own skill); installed count; computer switcher | `KUI/Marketplace/MarketplaceView.swift`, `MarketRows.swift`, `AgentIcon.swift` | Install agents, connectors, apps, skills | Reached from Profile menu > Computers > Extensions (and `kith://plugins`) | OPEN: needs a home (Extensions is not mentioned in DESIGN.md; proposal: Computers) |
| I-F2 | Installed view (connectors with Sign in, skills, remove confirm) | `KUI/Marketplace/InstalledView.swift` | Manage installed | same as I-F1 | OPEN (see I-F1) |
| I-F3 | Agent sheet (install an agent, Node.js check, API keys, steps) and Setup terminal | `KUI/Marketplace/AgentSheet.swift`, `SetupTerminal.swift` | Install a backend | same as I-F1 | OPEN (see I-F1) |
| I-F4 | Skill sheet, New skill sheet | `KUI/Marketplace/SkillSheet.swift`, `NewSkillSheet.swift` | Read, add, write skills | same as I-F1 | OPEN (see I-F1) |
| I-F5 | Install connector sheet, Custom connector sheet (Command / URL / Config; headers; environment) | `KUI/Marketplace/InstallConnectorSheet.swift`, `CustomConnectorSheet.swift`, `ConnectorSignInFlow.swift` | Add MCP servers | same | OPEN (see I-F1) |
| I-F6 | Composio views: app connect flow, key setup, "Open the sign-in page", waiting states | `KUI/Marketplace/ComposioViews.swift` | OAuth apps | same | OPEN (see I-F1) |
| I-F7 | Connection request card in chat (username/email field, "Saved securely on this computer") | `KUI/Marketplace/ConnectionRequestCard.swift` | Agent asks for a credential | Decision card | adapt |
| I-F8 | Credentials sheet: Storage, Saved connections, 1Password (service account), Sign-ins | `KUI/Marketplace/ConnectionRequestCard.swift` (`CredentialsView`) | Credential vault | same as I-F1 / Settings | OPEN (see I-F1) |

### I-G. Routines

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-G1 | Routines list in Details (plus, ask the bot for a routine) | `KUI/Routines/RoutinesView.swift` | List per bot | Agent sheet > Details | keep |
| I-G2 | Routine editor: name, instruction, schedule (cron, time zone, next run, validation, retry), webhook (URL, key show/hide/replace, copy), run settings (timeout), test run, delete (confirm), offline state | `KUI/Routines/RoutineEditorView.swift`, `RoutineWebhookPanel.swift`, `KK/Models/Routine.swift` | Create and edit | stock sheet from Agent sheet | keep |

### I-H. Call

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-H1 | Call screen: avatar, name, status, provider ("Voice by X"), timer, captions, failure/notice line | `KUI/Call/CallScreen.swift`, `CallTranscript.swift` | Full-screen voice call | Call (DESIGN.md only places the call button; no call-screen mock exists) | OPEN: needs a home (keep layout, restyle with tokens) |
| I-H2 | Call controls: Mute, speaker/earpiece, audio output picker, call settings, End call | `KUI/Call/CallScreen.swift` | Round controls | Call | keep |
| I-H3 | Engines (on device, Kith voice, OpenAI Realtime, Gemini Live, cloud speech) and call operator | `KUI/Call/*Engine*.swift`, `CallOperator.swift`, `SpeechStream.swift` | Voice back-ends | none (invisible) | keep |
| I-H4 | Call entry points: chat header phone, Agent sheet capsule | `ThreadView.swift`, `BotDetailsView.swift` | Starts a call | Phone beside the dock (home/Board) and top right of chat | adapt |

### I-I. Computer screen (remote)

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-I1 | Screen view overlay: close, "<agent> is using the computer", rotate, keyboard, touch mode (trackpad/direct), clipboard menu (send, copy from computer), fit screen, displays menu, take over / hand back, view-only banner, error with Try again | `KUI/Screen/ScreenView.swift`, `ScreenCanvas.swift`, `ScreenSession.swift` | Watch and control the Mac | None in DESIGN.md | OPEN: needs a home (entry today is chat toolbar Computer button, Computers row, `kith://screen`) |

### I-J. Outside the app: widgets, Live Activities, notifications, links

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-J1 | Widgets: Bots (small/medium/large + lock screen circular/rectangular/inline), Team (4 bots), Usage limits, Provider usage, Claude Code, Codex | `Widgets/KithWidgets.swift`, `KK/Design/WidgetCards.swift`, `AccessoryWidgetCard.swift` | Home and Lock Screen widgets | Restyle with tokens; bots keep avatars without tiles | keep |
| I-J2 | Lead-bot Control widget | `Widgets/KithWidgets.swift` (`LeadBotControl`) | Control Center button | none | keep |
| I-J3 | Live Activity and Dynamic Island (compact, minimal, expanded) | `Widgets/KithWidgets.swift` (`BotLiveActivity`), `KK/Design/ActivityCards.swift`, `App/Push.swift` | Task progress outside the app | none | keep |
| I-J4 | Widget gallery / setup: sample previews, notifications setup, widget check, size choice, "Choose the look", Lock Screen help; Live Activity and Island toggles with iOS-settings fallback | `Views/WidgetGalleryView.swift`, `ActivityGalleryView.swift`, `WidgetSetupDemo.swift` | Teaches and switches on widgets | None in DESIGN.md (State tab goes away) | OPEN: needs a home (same as I-A2) |
| I-J5 | Usage limits sheet (computer picker, per provider percentages, collapse; empty "No usage yet") | `Views/RootView.swift` (`UsageSheet`), `Views/UsageView.swift` | Usage limits (`kith://usage`) | None in DESIGN.md | OPEN: needs a home (header button lives on the State tab today; proposal: Profile menu > Usage) |
| I-J6 | Push notifications with actions: Open conversation, Review request; categories done, needsInput, failed; service extension for rich content | `App/KithApp.swift`, `App/Push.swift`, `NotificationService/NotificationService.swift` | Alerts | none | keep |
| I-J7 | Deep links: `kith://pair`, `bot`, `plugins`, `usage`, `computers`, `screen`; OAuth return | `App/KithApp.swift` (`open`) | Entry points | none; targets must land on new screens | keep (re-point `usage`, `plugins`, `computers`) |

### I-K. Shared controls

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| I-K1 | Living bots: CharacterAvatar / LivingBotView, ThinkingOrb, ProviderMascot, spare-cloud art | `KK/Design/CharacterAvatar.swift`, `LivingBotView.swift`, `LivingBotLogic.swift`, `LivingBotTint.swift`, `ThinkingOrb.swift` | The crew bots and their states (working, needs you, idle, asleep) | Bot avatar | keep (as they are; only the tile behind them goes) |
| I-K2 | Custom sheet, dialog, dropdown menu, long-press menu, segmented choice, toggles, card sections | `KUI/Chrome.swift`, `Controls.swift`, `Dialog.swift` | Own implementations of system controls | "Reuse stock iOS controls before drawing anything custom"; Liquid Glass for floating controls | adapt (swap for stock iOS 26 where possible) |
| I-K3 | Theme palette, motion | `KK/Design/Theme.swift`, `Motion.swift` | Colours and animation | Tokens | replace (tokens PR) |

## Desktop app (Electron + React)

Path shorthand (all relative to repo root):
- `M/` = `apps/desktop/src/main/`
- `V/` = `apps/desktop/src/renderer/views/`
- `C/` = `apps/desktop/src/renderer/components/`
- `S/` = `apps/desktop/src/renderer/store/`
- `SH/` = `apps/desktop/src/shared/`

Windows are routed by `?window=` in `apps/desktop/src/renderer/main.tsx`: `chat` (default), `pairing`, `island`. 

### Windows and app shell

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-1 | Main chat window | `M/index.ts` `openChat`; `V/ChatWindow.tsx` `ChatWindow` | 1100x760 (min 760x500) window, hidden title bar on macOS; closing hides it (app keeps running as a menu bar app); roster left, conversation right. | Desktop shell: Crew home with top bar, dock at the bottom | adapt |
| D-2 | Pair iPhone window | `M/index.ts` `openPairing`; `V/PairingWindow.tsx` `PairingWindow` | Fixed 340x420 non-resizable window titled "Pair iPhone" showing a one-time QR. Re-opening focuses it and re-sends `reviewApprovals` (reloads the code). | Computers (Profile menu), Pair a computer | keep |
| D-3 | Notch island window | `M/island.ts` `Island`; `V/island/IslandWindow.tsx` | Transparent always-on-top panel (720x300) at top-centre of the notch display; click-through except over the island shape; off by default (experimental). | None in DESIGN.md (island is a desktop-only surface) | OPEN: needs a home (keep as is, restyle bots without tiles) |
| D-4 | Call screen (not a window) | `V/call/CallView.tsx`, `V/call/CallScreen.tsx` | Voice call is an in-window overlay inside the conversation, not a separate BrowserWindow (no call window exists). | Call (overlay inside agent chat) | keep |
| D-5 | Single-instance + second-instance | `M/index.ts` (`requestSingleInstanceLock`, `second-instance`) | A second launch focuses the chat window or hands over a `kith://` URL; dev prints a hint to use `--name`. | n/a (app lifecycle) | keep |
| D-6 | Deep links / URL schemes | `M/auth.ts` `registerSchemes`, `handleURL`, `authenticate` | Registers `kith://` (and `kith-dev://`/appId) as default protocol client; used only to return browser sign-ins (connector OAuth `kith://oauth`, account `kith://callback`); 10 min timeout "Sign-in timed out." | n/a (sign-in return links) | keep |
| D-7 | App (menu bar) menu, macOS | `M/index.ts` `menu()` (`role: appMenu`) | Standard macOS app menu (About, Services, Hide, Quit). | n/a (native menu) | keep |
| D-8 | File menu | `M/index.ts` `menu()` (`role: fileMenu`) | Standard File menu (Close Window). | n/a (native menu) | keep |
| D-9 | Edit menu | `M/index.ts` `menu()` (`role: editMenu`) | Standard Undo/Redo/Cut/Copy/Paste/Select All. | n/a (native menu) | keep |
| D-10 | View menu | `M/index.ts` `menu()` | Items: Bigger (Cmd+=), Smaller (Cmd+-), Actual Size (Cmd+0), New Chat (Cmd+N), Search Bots (Cmd+F), Toggle Sidebar (Ctrl+Cmd+S), Conversation Details (Alt+Cmd+I), Toggle Full Screen; dev builds also Toggle DevTools and Reload. | Native menu; New Chat opens the new-chat menu, Search focuses the chat box ("Find or ask…"), Toggle Sidebar maps to Crew rail | adapt |
| D-11 | Window menu | `M/index.ts` `menu()` (`role: windowMenu`) | Standard Minimize/Zoom/etc. | n/a (native menu) | keep |
| D-12 | Dock icon visibility | `M/dock.ts` `applyDockVisibility`, `setShowInDock` | macOS only: hide/show the Dock icon (pref `showInDock`, default shown). No Dock context menu and no badge/bounce is set anywhere. | Settings > System | keep |
| D-13 | Launch at login | `M/index.ts` IPC `app:setLaunchAtLogin` | Toggles `openAtLogin` login item (from Settings > General and the tray). | Settings > System | keep |
| D-14 | Theme / background sync | `M/index.ts` `nativeTheme.on('updated')` | Re-colours opaque windows (#0A0A0A/#FFFFFF) when system appearance changes; island is left transparent. | Tokens (light, dark) | adapt |
| D-15 | Microphone permission handler | `M/index.ts` `setPermissionRequestHandler` | Only `media` and `clipboard-sanitized-write` permissions are granted; everything else denied. | n/a (permissions) | keep |
| D-16 | Native file picker (attach) | `M/index.ts` IPC `app:pickFiles` | Open dialog, multi-select files, for the composer's "+" button. | + in the chat box (Photos/Files) | keep |
| D-17 | Native folder picker | `M/index.ts` IPC `app:pickFolder` | Open-directory dialog with "Choose" button (default path optional). | Place chip "Choose a folder…" | keep |
| D-18 | Native save dialog (downloads) | `M/file-saves.ts` `registerFileSaves` | "Save" dialog for files a bot shares; streamed to disk via FileSink, cancellable. | File download card | keep |
| D-19 | SSH key chooser dialog | `M/ssh.ts` (`dialog.showOpenDialog` in `~/.ssh`, hidden files shown) | Native picker behind "Choose key file" in the SSH editor. | Computers > SSH computer editor | keep |
| D-20 | Reset all data flow | `M/index.ts` IPC `app:resetAllData` | Unclaims/revokes devices, signs out everywhere, uninstalls host, deletes data dirs, relaunches. Triggered from tray "Reset all data..." and confirmed in a dialog (see #21). | Settings > Start over | keep |
| D-21 | "Reset all data?" confirm dialog | `V/ChatWindow.tsx` `ChatWindow` (`Dialog`) | Destructive "Reset everything"; message: signs out every account, deletes bots, conversations, keys, settings; restarts at welcome. | stock alert | keep |
| D-22 | Error report plumbing | `M/error-reports.ts` `registerErrorReports` | Uncaught errors, renderer/child process crashes are sent to the host `reportError`; no UI. | n/a (invisible) | keep |
| D-23 | Quit lifecycle | `M/quit-lifecycle.ts` `QuitLifecycle` | Window close = hide unless quitting (incl. installer quit); installer-driven quit allowed. | n/a (lifecycle) | keep |
| D-24 | External link handling | `M/index.ts` `setWindowOpenHandler`, IPC `app:openExternal`/`app:openSettings`/`app:openMemoryFile` | New-window links go to the browser; only http(s)/mailto/System Settings URLs and `shared-memory`/`bot-memory` `.md` files may be opened. | n/a (links) | keep |
| D-25 | Website demo build | `apps/desktop/src/renderer/demo/*`, `apps/desktop/vite.demo.config.ts` | Renderer on an in-memory fake host (4 bots, Ship room group, a pending approval) served at /demo/desktop/, always dark. Not part of the Electron app. | n/a (website demo; must be re-skinned when the app is, or left on the previous design) | OPEN: needs a home (decide whether the website demo follows the redesign) |
| D-26 | Dev-only UI helpers | `V/ChatWindow.tsx` (`window.kith.debugOpen`), `M/index.ts` (`KITH_REMOTE_DEBUG`, `KITH_SHOW_INACTIVE`, `KITH_DEBUG_OPEN`) | Dev builds can auto-open compose / group / plugins / computers / a named bot; remote debugging port; show window inactive. | n/a (dev only) | keep |

### Tray (menu bar icon)

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-27 | Tray icon + alert variant | `M/tray.ts` `Tray.icon`, `update` | Template glyph on macOS/Linux, app icon on Windows (left click opens chat there); alert glyph when any bot needs attention. Tooltip = app name. | Menu bar icon (not covered by DESIGN.md) | keep |
| D-28 | Tray status line | `M/tray.ts` `rebuild` | Disabled first row: "Host not found" / "Host not installed" / "Connecting..." / "Host problem" / summary ("A device asks for access", "A bot needs you", "N bots working", "Connected"). | Status chips (Live, Mac unreachable) | adapt |
| D-29 | Open Kith | `M/tray.ts` `rebuild` | "Open Kith" (Cmd+O) opens/focuses the chat window. | Menu bar | keep |
| D-30 | Pair iPhone... | `M/tray.ts` `rebuild` | Only when host running; enabled if a local computer exists; opens pairing window. | Computers > Pair iPhone | keep |
| D-31 | Host state items | `M/tray.ts` `rebuild` (switch on host state) | missingBinary: dev "Rebuild Kith Dev..." or "Reinstall Kith or install the host with Homebrew." + "Copy host install command"; notInstalled: "Install host"; starting: "Starting the host..."; failed: error text + "Restart host" + "Open log". | Computers (host state) and status chips | keep |
| D-32 | Review access request | `M/tray.ts` `rebuild` | "Review access request from <device>..." when a device approval is pending; opens Settings approval sheet. | Computers (access request) | keep |
| D-33 | Bot list with submenus | `M/tray.ts` `rebuild`; `V/tray-summary.ts` `useTraySummary` | Each bot (with 20px rendered avatar + unread/needs-you badge) has a submenu: detail line, "Open conversation", and "Stop task" while working. "No bots yet" when empty. Groups are excluded. | Crew list rows (per-agent submenu stays in the menu bar) | keep |
| D-34 | Remote screen submenu | `M/tray.ts` `screenMenu` | "Enable remote screen" checkbox, subtitle line (e.g. "Your phone is viewing", "Needs approval in System Settings"), "Set up computer access...", error line. | Computers > Computer access | keep |
| D-35 | Usage block in tray | `M/tray.ts` `rebuild`; `V/tray-summary.ts` (`barIcon`, `usageLine`) | Per provider: logo row and one row per limit window with a rendered progress bar image (amber from 70%, red from 90%) and "<window> - N% - resets in ...". Click opens the app. | Settings > Usage | keep |
| D-36 | Settings submenu | `M/tray.ts` `settingsMenu` | Usage icons (Character/Original radio), Text Size (11-18 pt radio), Open at login, Show in Dock (macOS), Show in notch / Show at top of screen (island), Updates submenu, Restart host, Open log, Uninstall host service, Reset all data.... | Settings (same rows as the Settings page) | keep |
| D-37 | Tray Updates submenu | `M/tray.ts` `updatesMenu` | Primary action label (Check/Update to X/Downloading N%/Restart to update), "Waiting for paired iPhones to get a newer app", Automatically check / Automatically download and install checkboxes, "Updates are available in release builds.", Last checked, error + "Retry installing update". | Settings > Updates | keep |
| D-38 | Version + Quit | `M/tray.ts` `rebuild` | Disabled "Version X" row; "Quit Kith" (Cmd+Q). | Menu bar | keep |

### Sidebar, roster and navigation

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-39 | Split layout + resizable sidebar | `V/ChatWindow.tsx` `ChatSplitView`; `C/Controls.tsx` `ResizeHandle` | Sidebar drag-resizable (260 to 420); dragging narrower than 180 collapses to a 76px icon rail; arrow keys on the handle resize by 16px; widths persisted in prefs. | Crew list on the left, resizable; rail is the collapsed form | adapt |
| D-40 | Sidebar header actions | `V/ChatWindow.tsx` | Computer filter dropdown, Search (magnifier) and New (+, disabled when no computer online). | Top bar: filter becomes Profile > Computers; Search moves into the chat box; New becomes the new-chat menu | replace (top bar + chat box) |
| D-41 | New menu | `V/ChatWindow.tsx` `newItems` (`AnchoredMenu`) | "New chat" and "New group chat". Also on the rail "+" button. | New-chat menu: New bot, Invite a person, New group chat | adapt |
| D-42 | Compact rail | `V/ChatWindow.tsx`; `V/BotRoster.tsx` (`compact`) | Avatar-only roster; rail buttons: Search, New, Extensions, Update badge, Account avatar. Toggled by View > Toggle Sidebar, drag, or Settings > Compact sidebar. | Collapsed Crew rail | adapt |
| D-43 | Bot roster rows | `V/BotRoster.tsx`, `V/BotRow.tsx` `BotRow` | Avatar with status badge, pin glyph, bold name when unread, "using the computer" glyph, unread count badge, preview line: orb + activity (working), amber "Needs your approval", red error text, or last message / "<agent> - <folder>" / "N bots". | Crew row (bot 48, Nunito name, time/state, one sentence, ink count badge) | adapt |
| D-44 | Roster keyboard navigation | `V/ChatWindow.tsx` `onListKey` | Up/Down moves selection; Alt+Up/Down moves the selected bot within its list. | Crew list keyboard navigation | keep |
| D-45 | Drag to reorder bots | `V/BotRoster.tsx`; `renderer/lib/pointer-drag.ts` | Press-drag lifts a translucent ghost row, edge auto-scroll, rows slide, drops into new order (`store.reorder`); pinned/unpinned do not mix; Escape/blur cancels. | Crew list (manual order vs. design's fixed sort) | OPEN: needs a home (same question as iOS I-A8) |
| D-46 | Roster right-click context menu | `V/BotRoster.tsx` (`onContextMenu`); `V/ChatWindow.tsx` `botActions`; `V/PanelMenus.tsx` `DesktopActionMenu` | Pin/Unpin, Mark as Read, Edit Profile..., Copy conversation ID, New session, Hide from sidebar, Delete. Keyboard-driven (arrows/Tab/Enter/Esc). | Crew long-press / right-click menu: pin, colour, sleep, share (+ keep Mark as read, Edit profile, Copy ID, New session, Hide, Delete) | adapt |
| D-47 | New session confirm | `V/ChatWindow.tsx` (`Dialog`) | "Start a new session?" - conversation stays, agent starts with fresh context. | stock alert from ••• menu | keep |
| D-48 | Delete bot confirm | `V/ChatWindow.tsx`, `V/bots/GroupEditorView.tsx` `DeleteBotDialog` | "Delete <name>?" - "Delete bot and its conversation" (destructive). Group variant says its bots/chats stay. | stock alert | keep |
| D-49 | Roster empty states | `V/ChatWindow.tsx` | "No bots yet / Use + to start a new chat." or "Waiting for a computer". | Crew empty state | adapt |
| D-50 | Per-computer roster sections | `V/ComputerRosterSection.tsx`, `V/ComputerBadge.tsx`, `S/computer-roster.ts` | When more than one computer: collapsible heading (badge, name, connection label), drag to reorder sections, Alt+Up/Down moves; empty section shows "No bots yet" or the connection label. | Rooms, not machines; computer sections move to Profile > Computers | adapt |
| D-51 | Computer badge | `V/ComputerBadge.tsx` `ComputerBadge` | SF-symbol/Linux icon by device kind (laptop, Mac mini/Studio, iMac, Mac Pro, Linux) tinted by an 11-colour per-computer palette. | Computers | keep |
| D-52 | Computer filter dropdown | `V/ComputerFilterHeader.tsx` | Summary text ("N connected", "N/M connected", "Connecting...", "No access", "Not paired", "N offline"); menu: All computers, per-computer checkable rows with status, pending SSH profiles, "Only <name>", Reconnect, Install host on this Mac, Try again on this computer, Manage computers. | Profile > Computers (filter and Reconnect) | replace (Profile menu > Computers) |
| D-53 | Update-needed card (roster) | `V/UpdateNeededCard.tsx` | Per mismatched computer in roster (and replacing composer in thread): "Update this app" (shows UpdateButton) or "Update Kith on <host>" with OS-specific instructions or "Update <host>" restart button. | Status chip + Computers card (full text kept) | adapt |
| D-54 | Sidebar footer | `V/SidebarFooter.tsx` | Account avatar button, "Extensions" button with Gmail/Calendar/Drive logos, Update badge, fade over roster. | Profile menu (account) + Computers/Extensions entries | adapt |
| D-55 | Update badge | `V/UpdateButton.tsx` `UpdateBadge` | Down-arrow round button when a release is available; opens "Update to Kith X?" dialog ("Kith restarts to install it."). Hidden if unsupported or waiting for iOS app. | Status chip / Settings > Updates | adapt |
| D-56 | Account panel | `V/PanelMenus.tsx` `AccountPanelLayer` | Floating menu: Usage, Support (sub-page: Help & documentation, Report an issue -> GitHub), Settings (with pending-approval count), then Sign out or Continue with Apple/Google/GitHub; identity row (email or "Not signed in"); inline sign-in error. | Profile picture menu: account, Computers, Settings; Usage and Support rows keep a place under Settings | adapt |
| D-57 | Profile avatar | `V/PanelMenus.tsx` `ProfileAvatar` | Account photo or person glyph, spinner while account is busy. | Profile picture (piece of sky until a photo is set) | adapt |
| D-58 | Search palette | `V/SearchPalette.tsx` `SearchPalette` | Cmd+F / magnifier: "Search" field matching name, description or last message; Up/Down/Enter; Cmd+1..9 (Ctrl on Win/Linux) opens the Nth result; "No matching bots"; Esc/dim closes. | Chat box search ("Find or ask…", results in place) and search palette on Cmd+F | replace (search A: box filters the list) |
| D-59 | Empty detail pane | `V/ChatWindow.tsx` | Three animated crew bots, "Your coding agents, as teammates." (or "Connecting to your computers..."), "New Bot" button. | Crew empty detail state (the living bots stay) | adapt |
| D-60 | New chat compose page | `V/bots/NewChatView.tsx` `NewChatView` | "To:" chip field (search or create bots, Backspace removes chip), Cmd+1 = Create new Bot / `Create "<q>"`, Cmd+2..9 pick matches, Esc closes, starting-folder chip for single bot, message box; one recipient opens its chat, several create a group. | Chat box + New chat menu (New bot / crew) with place chip | adapt |
| D-61 | New group chat compose | `V/bots/NewChatView.tsx` (`group`) | Same page, needs at least two bots ("Pick at least two bots"); groups excluded from picker. | New group chat (new-chat menu) | adapt |
| D-62 | Computer picker for new chat | `V/ChatWindow.tsx` `ComputerPicker` | "On <computer>" dropdown when more than one computer is online. | Place chip "Mac" row / Computers | adapt |
| D-63 | Startup recovery | `V/StartupRecovery.tsx` `StartupRecovery` | When no computer is online but SSH profiles exist: "Connecting to your computers..." / "Your remote computers", SSH rows, Manage computers, host state card. | Crew home error and loading state (Mac unreachable chip) | adapt |
| D-64 | Host state / empty states | `V/HostStateView.tsx` `HostStateView`, `EmptyState` | "Set up this Mac" (+ Install host), "The host is missing", "The host isn't running" (+ Try again), "Starting the host... This takes a few seconds." | Computers / host state | keep |
| D-65 | Global error dialog | `V/ChatWindow.tsx` (`Dialog` bound to `app.errorMessage`) | "Something went wrong" with OK. | stock alert | keep |

### Onboarding, account and welcome

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-66 | Account welcome (first run) | `V/AccountWelcomeView.tsx` `AccountWelcomeView` | Animated crew (builder/designer/coder/lab/house), animated chat glimpse, "Your coding agents, as teammates."; buttons: Continue on this Mac, Continue with Apple/Google/GitHub; "Sign-in isn't configured in this build" and error text. | First run (not in DESIGN.md) | OPEN: needs a home (restyle with sky and crew; keep copy and sign-in buttons) |
| D-67 | Restoring-session splash | `V/AccountWelcomeView.tsx` | "Checking for a saved account..." spinner with "Continue on this Mac" skip; signed-in restore skips the screen. | First run loading | OPEN: needs a home (as #66) |
| D-68 | Computer access onboarding | `V/computer-access/ComputerAccessView.tsx`; `V/ChatWindow.tsx` | Shown once after welcome (pref `computerAccessSetup`): "Computer access" steps with Continue / Set up later. | First run / Computers > Computer access | keep |
| D-69 | Computer access (Settings / sheet) | `V/computer-access/ComputerAccessView.tsx`; `M/screen.ts`; `SH/computer-access.ts` | Rows with Ready/Needs access/Waiting badges: Kith Screen (Enable Kith Screen / Start again / Open Login Items), Screen recording (Allow screen recording + Open System Settings), Control apps (Allow computer control), Linux "Screen sharing & control"; Windows single "Computer control" row; notices "Waiting for your computer", "Kith Screen is missing", "Couldn't finish setup"; "Check again"; "N of M ready". Also opened from tray "Set up computer access...". | Computers > Computer access | keep |
| D-70 | Star on GitHub prompt | `V/StarPrompt.tsx`, `renderer/lib/star-ask.ts` | Sheet shown after setup (host online), again one week later; "Star Kith on GitHub" / "Enjoying Kith?", Not now / Star on GitHub; max two asks. | None in DESIGN.md | OPEN: needs a home (Star prompt; proposal: a one-time stock sheet after setup) |

### Conversation (thread)

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-71 | Thread view | `V/thread/ThreadView.tsx` `ThreadView` | Endless conversation with a bot/group: top chrome, transcript, floating composer, side panel (details or replies); narrow windows (<680) move panels into sheets. | Agent chat (top bar, feed of threads, dock) | adapt |
| D-72 | Title pill | `V/thread/ThreadView.tsx` | Avatar + name (opens details); subtitle with connection icon/label when not online or needs update (Connecting, Offline, No access, Not paired, Needs update; tooltips "Connected through Cloudflare / over Wi-Fi or Tailscale / locally"). | Agent chat top bar + status chips | adapt |
| D-73 | Working-folder chip | `V/bots/PlaceFolderPicker.tsx` `PlaceFolderControl`, `PlaceFolderChip` | Chip under title showing the chat's folder ("inherited", missing warning); click opens picker, confirm dialog "Change the working folder?" when messages exist. | Place chip | adapt |
| D-74 | Call and details buttons | `V/thread/ThreadView.tsx`, `V/thread/MacChatButton.tsx` | Round buttons: "Voice call" (disabled with reason tooltip) and "Conversation details" (chevron). | Top bar phone + ••• pair; header picture+name opens agent sheet | adapt |
| D-75 | Transcript | `V/thread/ThreadView.tsx` `Transcript` | Loads newest 40, auto-follows bottom, scroll-up releases; "Load earlier messages" button / auto-load near top; time separators (Today/Yesterday/date); row-in animation. | Agent chat feed | keep |
| D-76 | Jump to latest | `V/thread/ThreadView.tsx` | Floating arrow button when scrolled away. | Agent chat feed | keep |
| D-77 | Intro cards | `V/thread/ThreadView.tsx` `IntroCard`, `GroupIntroCard` | Empty chat: avatar, name, "<agent> in <cwd>" / Personal workspace, description, hint about notifications; group: members + "@mention a bot to ask just that one." | Agent chat empty state | keep |
| D-78 | User bubble | `V/thread/ChatRows.tsx` `UserBubble` | Text + attachments; hover time; status: "Sending...", "Queued"/"Queued until this response finishes" + Unsend, "Failed to send" + Resend/Delete, "Not sent". | Transcript turn (user bubble, right) | adapt |
| D-79 | Agent bubble | `V/thread/ChatRows.tsx` `AgentBubble` | Markdown text or file cards; author label in groups; "Answered by a backup account" footnote. | Transcript turn (agent bubble, left) | adapt |
| D-80 | Message context menu (right-click) | `V/thread/ChatRows.tsx` `Bubble` (`onContextMenu`, `useContextMenu`) | Quick reaction strip (6 emoji) plus Copy, Reply in thread, Unsend (queued user), Show what it did (agent). | Long-press menu: Copy, Reply in thread, Correct this (new), Unsend, Show what it did, reactions | adapt |
| D-81 | Hover message actions | `V/thread/ChatRows.tsx` `MessageActions` | Add reaction (emoji strip), Reply in thread, More (Copy, Show what it did). | Same actions as the long-press menu, on hover (desktop) | adapt |
| D-82 | Reactions | `V/thread/ChatRows.tsx` `ReactionsRow`; `S/bot-store.ts` `QUICK_REACTIONS` | Chips under a message, click removes; quick set thumbs up, heart, laugh, tada, eyes, check. | Reactions | OPEN: needs a home (same as iOS I-C8) |
| D-83 | Thread chip | `V/thread/ChatRows.tsx` `ThreadChip` | "N replies" with author avatars, "N new" or last time; opens thread panel. | Thread chip (bot avatar, N replies, 1 new, time, place) | adapt |
| D-84 | Replies / thread panel | `V/thread/RepliesView.tsx` | Header "Thread" (chat name), folder chip, root message, "No replies yet"/"N replies", replies, working indicator, composer in thread mode; sheet when narrow. | Thread opened full screen / side panel | keep |
| D-85 | Task rows | `V/thread/TaskRow.tsx`, `SH/tasks.ts` | Tasks folded as cards (status icon, title, "N replies - day") or open inline with status (Working/Needs you/Stopped/Done) and buttons Stop this task, Continue this task, Fold this task. | Thread chip + open replies; Stop and Continue kept | adapt |
| D-86 | Working indicator | `V/thread/ChatRows.tsx` `WorkingIndicator` | Orb + activity ("Working..."/"Needs ..."), elapsed timer, click to unfold live thinking text. | Working indicator (status chip "Live") | adapt |
| D-87 | Permission card | `V/thread/PermissionCard.tsx` `PermissionCard` | Headline from tool kind (run command, change files, web, read files, tool), command title, "Runs on <host> - <folder>", Details disclosure (command, detail, diffs), options Allow once / Always allow / Deny / Never; resolved text (Allowed once, Denied, Cancelled, "Expired - the agent moved on"). | Decision card (Allow / Deny / Always) | adapt |
| D-88 | Diff and code boxes | `V/thread/PermissionCard.tsx` `DiffView`, `CodeBox` | Per-file diff (+/- counts, up to 80 lines, new-file icon). | Tool row sheet (command, output) and diff | adapt |
| D-89 | Notice rows | `V/thread/ChatRows.tsx` `NoticeRow` | Plain notice, divider, red error notice, call summary ("... - mm:ss"), routine notice button (opens routine in details). | Transcript notices; call summary | keep |
| D-90 | Connection / sign-in request card | `V/marketplace/ConnectionRequestCard.tsx` | In-chat card when a bot needs a connector, app, agent sign-in, login or credential; actions Connect/Sign in, username/password or credential fields ("Saved securely on this computer... never sees it"), final state Connected/Signed in/Saved/Cancelled. | Decision card | adapt |
| D-91 | Handoff notice | `V/thread/HandoffNotice.tsx` | Draft: "What it carries" disclosure, "Keep this conversation", Edit (opens "Edit the handoff" sheet with save); folded once decided. | Handoff card | adapt |
| D-92 | Bot-to-bot exchange rows | `V/thread/BotConversation.tsx` `BotMessageRow`, `BotConversationView` | Collapsed row between bots ("Open conversation", failed state); sheet with the full pair conversation, "No messages yet.", errors. | Group modes beyond DESIGN.md | OPEN: needs a home (same as iOS I-C15) |
| D-93 | File download card | `V/thread/FileDownloadCard.tsx`; `S/file-downloads.ts` | File name/size card; button Download / Cancel download / Retry download / Save again (native save dialog). | File download card | keep |
| D-94 | Attachment list + image viewer | `V/thread/Attachments.tsx` | Images as thumbnails opening a full-size sheet; other files as cards with byte size. | Attachments in bubbles | keep |
| D-95 | Markdown rendering | `V/thread/Markdown.tsx`, `V/thread/markdown-blocks.ts` | Inline code, bold, italic, strike, links (open in browser, URL tooltip), code blocks, tables with horizontal scroll, lists, headings. | Transcript turn (markdown) | keep |
| D-96 | "Full conversation" trace | `V/thread/TraceView.tsx` | Sheet listing narration, Thinking, tool calls with output as disclosures; "Nothing yet." | Tool row sheet / "Full conversation" in ••• menu | adapt |
| D-97 | Mark-as-read behaviour | `V/thread/reading.ts` `useReading` | Visible thread marks bot replies read (drives unread badges). | n/a (behaviour) | keep |
| D-98 | Side panel resize | `V/thread/ChatSidePanel.tsx`; `C/Controls.tsx` `ResizeHandle` | Details (292) and Replies (420) panels drag-resizable, min 260, keeps 380 for chat. | Desktop side panels (not in DESIGN.md) | keep |

### Composer

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-99 | Composer box | `V/thread/Composer.tsx` `Composer` | Auto-growing textarea (up to 8 lines), Enter sends, Shift+Enter newline, IME-safe; placeholders: "Ask <bot>", "Queue a message for <bot>" while working, "New task for <bot>", "Reply...", "Message everyone - @ to ask one bot". Per-destination persisted drafts. | Chat box (dock) | adapt |
| D-100 | Trailing button states | `V/thread/Composer.tsx` | Morphs Send / Stop (working, empty) / Call (waveform, empty, call allowed) / Interrupt (while bot speaks in a call). Disabled when offline unless queueing allowed. | Chat box: send / stop; call button right, hides while typing | adapt |
| D-101 | Attachments (+) | `V/thread/Composer.tsx`, `V/thread/ComposerAttachment.tsx` | "Add files" opens native picker; drag-drop files; paste copied files/images (clipboard files via main); chips with remove (images as thumbnail tiles); 100 MB limit error "<name> is larger than 100 MB." | + in the chat box | adapt |
| D-102 | Pasted-text marking | `V/thread/Composer.tsx` (`pasted`) | Pasted text is sent flagged as pasted so memory does not treat it as the user's words. | n/a (behaviour) | keep |
| D-103 | New task toggle | `V/thread/Composer.tsx` | Pencil button ("New task", pressed state "Your next message starts a new task") for task-enabled bots; reveals a starting-folder chip. | Redundant with new-thread-per-message; place chip replaces the folder choice | replace (every message starts a new thread; place chip) |
| D-104 | Starting-folder chip | `V/bots/PlaceFolderPicker.tsx` `StartingFolderChip` | Chooses the folder a new chat/task starts in (inherited default shown). | Place chip | replace (place chip) |
| D-105 | Slash commands | `V/thread/CommandMenu.tsx`, `V/thread/slash-commands.ts` | Typing `/` lists commands: `/fast` (Fast mode toggle with state detail or refusal warning) and installed skills; Up/Down, Tab/Return picks, Esc hides; typing `/<skill> ` expands on host. Groups have none. | `/` opens commands | keep |
| D-106 | @mention suggestions | `V/thread/Composer.tsx` | In groups, `@` shows member chips to insert `@Name `. | `@name` skips routing | keep |
| D-107 | Dictation | `V/thread/dictation.ts` `useDictation`; `M/speech.ts` | Mic button ("Dictate"/"Stop dictating") using on-device Swift recogniser (macOS), live partials appended to the draft; one recogniser at a time; stops on pause or chat change. Hidden where unavailable. | Mic in the chat box | keep |
| D-108 | Update-needed replaces composer | `V/thread/ThreadView.tsx` | Mismatched computer shows UpdateNeededCard in the composer's place. | Status chip in the dock | adapt |

### Voice call

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-109 | Call screen | `V/call/CallScreen.tsx` `CallScreen` | Full pane: big avatar with mic-level ring (tap to interrupt while speaking), name, state text (Connecting, Listening, Speaking, Muted, Working..., Downloading voice..., Can't listen), timer + "Voice by <provider>", notice/warning line, live transcript. | Call | keep |
| D-110 | Call controls | `V/call/CallScreen.tsx` | Round buttons: Call settings, Speaker, Mute/Unmute, End call. | Call | keep |
| D-111 | Speaker output menu | `V/call/CallScreen.tsx` `SpeakerButton` | Radio menu: System default plus enumerated audio outputs; disabled with tooltip for engines that cannot choose output. | Call | keep |
| D-112 | Captions | `V/call/CallScreen.tsx` `Transcript`; `V/call/transcript.ts` | You/bot lines; empty hints "What you say and what the bot says shows up here." or "Live captions are available with Kith voice and on-device voice." | Call | keep |
| D-113 | Call availability blocks | `V/call/call-availability.ts` | Disabled reasons: "Can't call while the computer is offline.", "Update Kith on the computer to call.", on-device unavailable + "Pick a cloud voice in call settings." | Call (disabled reasons) | keep |
| D-114 | Engine selection + fallbacks | `V/call/CallView.tsx`; `OnDeviceEngine`, `KithVoiceEngine`, `OpenAIRealtimeEngine`, `GeminiLiveEngine`, `CloudSpeechEngine` | On-device, Kith voice (local model), OpenAI/Gemini realtime or speech mode; falls back to on-device with notices ("<provider> isn't available (...). Using on-device voice."); blocked message if no key. | Call (invisible engines) | keep |
| D-115 | Call logging | `V/call/CallView.tsx` | Call duration logged as a notice in the chat ("... - mm:ss"); cloud minutes counted for the month. | Transcript notice | keep |
| D-116 | Call settings sheet | `V/call/CallView.tsx` (`Sheet` with `VoiceChatSettingsView`) | Opens Settings > Voice chat (or a local sheet) from the gear. | Settings > Voice | keep |

### Details panel, bot editor, memory, groups

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-117 | Details panel | `V/thread/DetailsPanel.tsx` `DetailsPanel` | Inspector: Create template and Close buttons, identity (avatar, name, agent + model, Kith header), Call button, tabs Details / Memory / Settings; slides in as panel (wide) or 340px sheet (narrow). | Agent sheet (Details / Memory / Settings) | adapt |
| D-118 | Details tab | `V/thread/DetailsPanel.tsx`, `V/bots/kith/KithDetails.tsx` | Kith sections (if hq agent), instructions + Agent/model rows, "Use the computer" switch (with setup hint), Routines list. | Agent sheet > Details | keep |
| D-119 | Computer-use switch | `V/thread/DetailsPanel.tsx` `ComputerUse` | "Use the computer" per bot; hint to finish Computer access setup. | Agent sheet > Settings | keep |
| D-120 | Group members list | `V/thread/DetailsPanel.tsx` `GroupMembersList` | Members open own chat, "Remove <bot> from the group" (if >1), "Add Member". | Group agent sheet / group editor | keep |
| D-121 | Create template sheet | `V/bots/BotTemplateView.tsx` | Pretty-printed JSON of bot settings; Copy template (Enter copies); excludes history and ids. | New bot (template) | keep |
| D-122 | Bot editor sheet | `V/bots/BotEditorView.tsx` `BotEditorView` | Modal "New bot"/"Settings" with Create/Save check, validation, spinner, error. | New bot flow; agent sheet > Settings | keep |
| D-123 | Bot settings form (autosave inspector) | `V/bots/BotEditorView.tsx` `BotSettingsPanel`, `BotSettingsForm` | Autosaves 600 ms after edits. Sections: Profile (Avatar, Name, Standing instructions), Instructions/Agent (Agent picker incl. "Custom command", ACP command field, "not installed" hints, Model, model options, Fast mode switch), Workspace (Computer, Workspace dropdown Personal / Choose project folder..., Default project dropdown, Permissions: Claude Code's six modes, default / acceptEdits / plan / auto / dontAsk / bypassPermissions), Activity (Notifications switch, Use the computer switch), Connectors toggles, Skills toggles with info button. | Agent sheet > Details / Settings | keep |
| D-124 | Avatar picker | `V/bots/BotEditorView.tsx` `AvatarPicker`, `V/bots/parts.tsx` `AnchoredPanel` | Popover: Automatic, Crew, Spares, Colour (original + 15 palette gradients); groups show "A group wears the Kith cube." | Colour sheet (Gradient / Plain, preset swatches, Reset to default) | adapt |
| D-125 | Model picker | `V/bots/AgentModelPicker.tsx` | Model dropdown (Default + list), refresh/retry, loading spinner, free-text model ID for custom/no-list, per-model options, failure/empty notes, "Changing the model starts a new agent session." | Agent sheet > Details | keep |
| D-126 | Folder picker (project folder) | `V/bots/BotEditorView.tsx` `FolderPicker`, `FolderLevel` | Drill-down host folder browser: search field, "Use "<folder>"", Locations, git-folder icons, back button, spinner, error. | Place chip "Choose a folder…" | keep |
| D-127 | Working folder sheet | `V/bots/PlaceFolderPicker.tsx` `PlaceFolderSheet` | "Working folder / Changing it starts a fresh session here.": Use default, Kith projects, Recent folders, Choose folder...; browse step with "Register as Kith project" (project-id field, Register and use / Use folder only); errors. | "Where coder works" sheet (Suggested, Projects, Choose a folder, Mac) | adapt |
| D-128 | Group editor | `V/bots/GroupEditorView.tsx` | "New group chat"/"Group chat": name, description, members with remove, Add Member search (Enter adds, Esc closes), "Everyone answers in turn unless you @mention someone.", Delete group chat. | New group chat / group editor | keep |
| D-129 | Memory tab (non-hq bots) | `V/bots/MemoryView.tsx` `MemoryCard`/`BotMemory` | Tabs About you / This bot / Tidy-ups (/ <harness> when kept); entries with pin, edit, archive, forget; Add memory sheet (Enter saves); More menu (Refresh, Forget everything here); "Use what your bots share" switch; harness-memory switch + info; "Forget everything...?" dialog; capacity note; empty states. | Agent sheet > Memory | keep |
| D-130 | Memory notes (files) | `V/bots/MemoryFiles.tsx` `fileRows`, `MemoryFileSheet` | Long-form notes: New note, rows with tokens/date and cap warnings; sheet to view, Edit, Open in your editor (local only), Delete, Versions with "Bring back this version"; file-name validation. | Agent sheet > Memory | keep |
| D-131 | Tidy-ups list | `V/bots/MemoryView.tsx` | Per-run adds/archives with Undo this tidy. | Agent sheet > Memory (history, Undo) | keep |
| D-132 | Harness memory info sheet | `V/bots/HarnessMemorySheet.tsx` | Explains the agent's own memory (4 info rows). | Agent sheet > Memory | keep |
| D-133 | Kith/hq memory tab | `V/bots/HqMemory.tsx` | For hq agents: About you (shared files, e.g. user.md), This agent (MEMORY.md + files, with memory search), Tidy-ups with "Undo this tidy?" dialog; file sheet with Edit/History tabs, Save, conflict banner (Reload / Save anyway), Restore this version / Undo this change dialogs, "Discard your changes?". | Agent sheet > Memory; history "You · corrected" with Undo | keep |
| D-134 | Agent memory search | `V/bots/kith/KithMemory.tsx` `AgentMemorySearch` | "Search this agent's memory" with Searching..., "Nothing found", hits that open Markdown files. | Agent sheet > Memory; also the "Search the crew's memory" row of the chat box | keep |
| D-135 | Kith details (hq agents) | `V/bots/kith/KithDetails.tsx` | About (description + collapsible Instructions), Cost (Today/7 days/30 days/All, requests, top models), Schedules (cron words, Paused, last/next run), Projects, Harness, Skills/Tools/MCP servers/Plugins groups, Refresh with "Read <ago>", Banner with Retry, per-section "Couldn't read ...". | Agent sheet > Details | keep |
| D-136 | hq permission row | `V/bots/kith/HqSettings.tsx` | Editable permission-mode picker (and read-only rows) from hq, with "Permission not changed: ..." error. | Agent sheet > Settings | keep |
| D-137 | Per-agent backups | `V/bots/kith/AgentBackups.tsx`; `V/settings/BackupChain.tsx` `ChainEditor` | "Use the global order" / "Custom order" segmented choice and chain editor. | Agent sheet > Settings, Settings > Backups | keep |

### Routines

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-138 | Routines list | `V/routines/RoutinesView.tsx` | In Details tab: header with "Ask the bot for a routine" (fills composer with "I want a routine that "), "Set up a routine" (+); rows with name, status (Queued/Starting/Running/Resuming after restart/Failed/Interrupted/"Schedule needs attention"/trigger text) and enable Switch; polls every 3 s; empty/loading/load-error text. | Agent sheet > Details (Routines) | keep |
| D-139 | Routine editor | `V/routines/RoutineEditorView.tsx` | Sheet "Set up a routine"/"Edit routine": Name, Instruction, When to run (Schedule / Webhook / keep original), cron + time zone fields with live preview (summary, next run, warning), "Run settings" disclosure (Timeout seconds), Save (Cmd+S) / Create routine, Delete routine, Test run, "Check schedule again", "Retry loading schedule", offline notice "Connect to this computer to save your routine." | Sheet from Agent sheet | keep |
| D-140 | Routine webhook panel | `V/routines/RoutineWebhookPanel.tsx` | URL (or Local URL) and masked Key with copy, show/hide, Replace key (confirm dialog); explanatory footnote about Bearer auth and 72 h queueing. | Sheet from Agent sheet | keep |
| D-141 | Delete routine dialog | `V/routines/RoutineEditorView.tsx` | "Delete routine?" destructive confirm. | stock alert | keep |

### Marketplace (Extensions)

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-142 | Extensions sheet | `V/marketplace/MarketplaceView.tsx` `MarketplaceView` | Large sheet from the sidebar: computer badge + title, computer switcher dropdown (multi-computer), Credentials button, "N installed" button, search ("Search agents, connectors and skills", Enter searches registry, clear button), footer note on sources. | Extensions (not in DESIGN.md) | OPEN: needs a home (as iOS I-F1) |
| D-143 | Agents section | `V/marketplace/MarketplaceView.tsx` `AgentCard` | Grid of agents with status (Not installed / Needs Node.js / Sets up on first use / Not signed in / Installed); "Load more agents". | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-144 | Node.js notice | `V/marketplace/AgentSheet.tsx` `NodeNotice` | "Node.js isn't on <host>" with Add Node.js. | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-145 | Agent setup sheet | `V/marketplace/AgentSheet.tsx` `AgentSheet` | Steps Install (installer), Node.js, Sign in with status, "Check again"; sign-in options (terminal, API key, browser auth); API-key entry sheet with "Get a key". | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-146 | Setup terminal | `V/marketplace/SetupTerminal.tsx` | Embedded terminal for Add Node.js / Install / Sign in; "Finished." / "Ended with an error (n)."; Done button. | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-147 | Connectors section | `V/marketplace/MarketplaceView.tsx` | MCP-registry connectors grid with Add / Added, skeleton loading, "No connectors match..." / "Couldn't load connectors.", "Load more connectors". | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-148 | Install connector sheet | `V/marketplace/ConnectorSheets.tsx` `InstallConnectorSheet` | "Add connector": website link, Runs (hosted vs on computer options), Setup inputs (secrets saved on host only). | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-149 | Custom connector sheet | `V/marketplace/ConnectorSheets.tsx` `CustomConnectorSheet` | Command / URL / Config segmented modes (paste MCP JSON), Name, target, Headers or Environment. | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-150 | Apps (Composio) section | `V/marketplace/ComposioViews.tsx` | "Apps": Composio key setup tile, app grid with Connect, "No apps match..." / "Couldn't load apps.", "Load more apps". | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-151 | Composio key sheet | `V/marketplace/ComposioViews.tsx` `ComposioKeySheet` | API key (ak_...), "Get a key from Composio", Remove key. | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-152 | Composio connect sheet | `V/marketplace/ComposioViews.tsx` `ComposioConnectSheet` | "Connect <app>": extra fields, "Asking Composio...", "Waiting for you to finish signing in...", failure message. | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-153 | Skills section | `V/marketplace/MarketplaceView.tsx` | Skills grid (Add / Added / busy, tap to open details). | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-154 | Skill sheet | `V/marketplace/SkillSheet.tsx` | SKILL.md instructions rendered as Markdown, Add / "Added", "No instructions." | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-155 | New skill sheet | `V/marketplace/ConnectorSheets.tsx` `NewSkillSheet` | Name, "When to use it", step-by-step instructions; Save. | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-156 | "Make your own" | `V/marketplace/MarketplaceView.tsx` | Rows: Custom connector, Your own skill (New). | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-157 | Installed view | `V/marketplace/MarketplaceView.tsx` `InstalledView` | "Connectors & skills": per-item Sign in (when needed), remove trash with "Remove <name>?" dialog; "Nothing installed" empty state. | Extensions | OPEN: needs a home (as iOS I-F1) |
| D-158 | Credentials view | `V/marketplace/CredentialsView.tsx` | Storage (system credential store, encrypted), Saved connections (edit keys sheet), 1Password service-account token (Connect/Disconnect), Sign-ins (site, username, password or op:// reference) list with remove. | Extensions > Credentials (or Settings) | OPEN: needs a home (as iOS I-F1) |
| D-159 | Slide-over navigation | `V/marketplace/pieces.tsx` `SlideStack`, `ScreenHeader` | Credentials/Installed slide over the Extensions page with Back. | Extensions | OPEN: needs a home (as iOS I-F1) |

### Settings

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-160 | Settings sheet shell | `V/settings/SettingsView.tsx` `SettingsView` | 800x620 sheet with sidebar groups Personal (General, Voice chat, Usage) and This Mac (Computers & devices, Computer access, Updates); pending-approval count next to Computers; Close button. | Settings (from Profile menu) | adapt |
| D-161 | General > Account | `V/settings/SettingsView.tsx` `GeneralSettings` | Email or "Not signed in", Sign out or Apple/Google/GitHub buttons, error footer. | Profile menu / Settings > Account | keep |
| D-162 | General > Link chat | `V/settings/LinkChatRow.tsx` | "Link Telegram or Discord": Link / New code, shows `/link NNNNNN`, Copy, expiry minutes. Hidden on older hosts. Below it, `ScheduleMessagesRow`: "Schedule changes in Telegram or Discord" switch (`scheduleMessages`, per person, on by default; the app card stays either way), hidden on older hosts. | Settings > Link chat | keep |
| D-163 | General > Appearance | `V/settings/SettingsView.tsx` | Text size (11 to 18 pt picker), Usage icons (Character/Original), Compact sidebar switch. | Settings > Appearance | keep |
| D-164 | General > System | `V/settings/SettingsView.tsx` | Show in Dock (macOS), Show in notch / Show at top of screen (island, if supported), Open at login. | Settings > System | keep |
| D-165 | General > Privacy | `V/settings/SettingsView.tsx` `PrivacySettings` | "Share usage analytics" switch (disabled when host not running) + info button. | Settings > Privacy | keep |
| D-166 | Analytics info sheet | `V/settings/AnalyticsSheet.tsx` | 7 explanatory rows (features, crashes, location, identity, never shared, usage, choice). | Settings > Privacy | keep |
| D-167 | Voice chat settings | `V/settings/VoiceChatSettingsView.tsx` | Engine (On device / Kith voice / OpenAI / Gemini), Mode (Realtime/Speech), Language, Reading speed (Slower/Normal/Faster), "Send after a pause of" (1/1.5/2.5 s); footers explain current engine; "Voice keys are kept by this Mac's host, which isn't running." when no local host. | Settings > Voice | keep |
| D-168 | Kith voice section | `V/settings/VoiceChatSettingsView.tsx` `KithVoiceSettings` | Voice model state (Ready/Downloading with progress/Failed/Unavailable/Not downloaded), Download/Retry, Voice picker, "About 65 MB" footer. | Settings > Voice | keep |
| D-169 | Provider voice section | `V/settings/VoiceChatSettingsView.tsx` `ProviderVoiceSettings` | API key entry/test/replace/remove, models (Model or Speech-to-text/Text-to-speech), Voice, "This month ~ N min". | Settings > Voice | keep |
| D-170 | Usage page | `V/settings/SettingsView.tsx` `UsageLimitsList`; `V/settings/UsageLimits.tsx`; `V/usage/*` | Per-provider bars (Session, Weekly, Opus ...) with percent and reset text, per-computer headings, "No usage information yet.", provider mascot with "!" when near limit. | Settings > Usage (also the iOS home for usage) | keep |
| D-171 | Backups (global) | `V/settings/BackupChain.tsx` `BackupChains` | Under Usage: ordered fallback accounts per computer; per entry state, why, terminal login command with copy; Move up/down, Remove, Add backup dropdown, Refresh; Claude-accounts note. | Settings > Backups | keep |
| D-172 | Computers & devices | `V/settings/ComputersView.tsx` | Managed computer cards, "Over SSH" list, "In your account" list (see rows 173-180). | Profile menu > Computers | keep |
| D-173 | Managed computer card | `V/settings/ComputersView.tsx` `ManagedComputerCard` | Header with status, Voice chat (SSH) and Pair iPhone buttons; "Reach from anywhere" switch with relay status line; account line with "Add to account"/"Remove from account"; "Skip the 6-digit check" switch (account owner only). | Computers | keep |
| D-174 | Authorized devices | `V/settings/ComputersView.tsx` | "Devices that can use <name>": name, source (account vs QR), connected/seen; Revoke access (confirm "Revoke <device>?"). | Computers | keep |
| D-175 | Pairing QR panel | `V/settings/ComputersView.tsx` `PairingPanel`; `V/PairingWindow.tsx` | QR, instructions, warnings when "Reach from anywhere" off, expiry time, New code, Copy pairing link, error + Try again. Auto-refreshes at expiry. | Computers > Pair iPhone | keep |
| D-176 | Confirm dialogs (computers) | `V/settings/ComputersView.tsx` | "Let account devices in without a code?" (Skip the check), "Remove <name> from the account?". | stock alerts | keep |
| D-177 | SSH computers | `V/settings/ComputersView.tsx`, `V/settings/SSHRow.tsx`, `V/settings/ssh-model.ts` | Add (+); row with status line (Not connected, step text, Retrying, failed message, not installed with install command + copy); Connect/Disconnect, Edit, Remove. | Computers | keep |
| D-178 | SSH host-key confirmation | `V/settings/SSHRow.tsx` | Fingerprints, `ssh-keygen -lf` hint, "Trust and connect" / "Cancel". | Computers | keep |
| D-179 | SSH profile editor | `V/settings/ComputersView.tsx` `SSHProfileEditor` | Add/Edit SSH computer: Name, Host, User, SSH port, Key (choose file / use ssh-agent), Kith port; port validation errors; Enter saves. | Computers | keep |
| D-180 | Account computers list | `V/settings/ComputersView.tsx` | Reached computers (route icon cloud/wifi, "Older copy of this computer", Remove from this Mac) and others in the account (Online/Offline); sign-in prompt / "No other computers in your account yet." | Computers | keep |
| D-181 | Access request sheet | `V/settings/ApprovalSheet.tsx` `ApprovalSheet` | "<device> wants to use <host>", signed-in email, 6-digit code to match (or waiting spinner), Deny / Approve; closing defers. Opens automatically. | Computers (access request) | keep |
| D-182 | Updates page | `V/settings/SettingsView.tsx` `UpdateSettings` | Version + UpdateButton, last checked, download progress bar, two auto-update switches, error with Retry, footer note when unsupported. | Settings > Updates | keep |
| D-183 | Open-source licenses sheet | `V/settings/LicensesSheet.tsx` | Intro and one group per license with Show/Hide packages and license text. | Settings > About | keep |

### Island

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-184 | Island modes | `SH/island.ts` `islandMode`; `V/island/island-model.ts` | hidden, peek, compact (wings), overview, empty, approval, finished, failed, message, usage; hover opens (peek then open), click pins with 10 s countdown ring; finished notice lasts 5.2 s. | Island (desktop-only surface) | OPEN: needs a home (as #3) |
| D-185 | Wings (compact/peek) | `V/island/IslandViews.tsx` `Wings` | Focus bot avatar + badge on left, others cluster or orb/dot on right, "Needs you" / "N working" label on screens without a notch. | Island | OPEN: needs a home (as #3) |
| D-186 | Island header | `V/island/IslandViews.tsx` `Header` | Tabs: Bots, Message a bot, Usage, New bot (+); right side "N working", usage meter, Open Kith. | Island | OPEN: needs a home (as #3) |
| D-187 | Overview | `V/island/IslandViews.tsx` `Overview` | Focus card (big character, agent name/backend, last 4 tool steps with icons) opening conversation; pills for other bots. | Island | OPEN: needs a home (as #3) |
| D-188 | Empty | `V/island/IslandViews.tsx` `Empty` | "Nothing running right now." with Message a bot / New bot. | Island | OPEN: needs a home (as #3) |
| D-189 | Approval card | `V/island/IslandViews.tsx` `Approval` | Amber card with bot, headline, command, buttons ordered Never/Deny/Always allow/Allow once with global shortcut hints Alt+Y / Alt+N; spinner while answering; mini column of other bots. | Island; permission uses the Decision card styling | OPEN: needs a home (as #3) |
| D-190 | Finished / failed notice | `V/island/IslandViews.tsx` `Notice` | Green "<agent> is done" or red "<name> stopped." with error; Open conversation, OK. | Island | OPEN: needs a home (as #3) |
| D-191 | Message tab | `V/island/IslandViews.tsx` `Message` | Bot chip (click cycles bot), "<backend> - <folder>", input "Message <bot>...", Enter sends, Esc closes; takes keyboard focus only here. | Island | OPEN: needs a home (as #3) |
| D-192 | Usage tab and meter | `V/island/IslandUsage.tsx` `UsageView`, `UsageMeter` | Tightest limit as logo + bar + percent in header (opens Usage); Usage view shows up to 2 providers x 3 windows; "No usage yet." | Island | OPEN: needs a home (as #3) |
| D-193 | Global shortcuts | `M/island.ts` `setKeys` (`globalShortcut`) | Alt+Y allow / Alt+N deny registered system-wide only while an approval shows. | Island | OPEN: needs a home (as #3) |
| D-194 | Island positioning | `M/island.ts` `place`, `readScreens` | Finds notch display via JXA/AppKit, follows display changes; Linux X11 uses window shape; Wayland unsupported (setting hidden). | Island | OPEN: needs a home (as #3) |

### Living bots, avatars and shared components

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-195 | Living avatars | `C/Avatar.tsx` `LiveBot`; `C/living-bots-art.ts`; `C/living-bots-window.ts`; `SH/living-bots.ts` | Layered crew bodies with code-drawn eyes, CSS-only loops (idle blink/glance, working, needs-you double hop, done one-shot, error), seeded by bot id, recoloured by palette (OKLab); IntersectionObserver freezes off-screen avatars; cap on full-tier avatars; pause when hidden. | Bot avatar (living bots stay as they are) | keep |
| D-196 | Avatar status badges | `C/Avatar.tsx` `AvatarWithStatus` | Amber "!" when needs input, dot when unread. | Bot avatar states: needs you (looks up), ink count badge on rows | adapt |
| D-197 | Group avatar | `C/Avatar.tsx` `GroupAvatar` | Kith cube tile. | Group row: two bots on the diagonal + one sky patch | adapt |
| D-198 | Crew portrait | `C/Avatar.tsx` `CrewPortrait`; `SH/crew.ts` | Static portraits in the avatar picker. | Colour sheet (preview) | adapt |
| D-199 | Thinking orb | `C/ThinkingOrb.tsx` | Animated orb for working/listening states. | Working indicator | keep |
| D-200 | Overlay system | `C/Overlay.tsx` `Sheet`, `Dialog`, `AnchoredMenu`, `MenuPanel`, `ModalHeader`, `useEscape` | Modal cards over dim (Esc closes top layer only), alertdialog with actions/cancel, anchored popup menus (radio/check/destructive/divider, reaction strip), context-menu hook. | Stock-like sheets, menus (Liquid Glass equivalents on desktop) | adapt |
| D-201 | Controls | `C/Controls.tsx` | Button, IconButton, Switch, DropdownMenu, ChoicePicker, SegmentedChoice, TabChoice, SearchField, Disclosure, ProgressBar, Spinner, CardForm/CardSection, ResizeHandle. | Tokens and components | adapt |
| D-202 | Icons | `C/Icon.tsx`, `C/icon-fallbacks.ts`, `C/AgentIcon.tsx`, `C/GitHubMark.tsx` | SF-symbol-style icon set with fallbacks; per-agent logos. | Icons (SF Symbols on Apple platforms; desktop icon set stays SF-style) | keep |
| D-203 | Text size + theme | `renderer/lib/prefs.ts`, `renderer/lib/theme.ts` | 11 to 18 pt text size (View menu, tray, Settings); light/dark follows system; reduce-motion respected. | Tokens; Settings > Appearance | keep |

### Main-process-only behaviours with no UI of their own

| ID | Screen / feature | Code | What it does today | New home | Verdict |
|---|---|---|---|---|---|
| D-204 | App updater | `M/updates.ts` `Updates` | electron-updater from GitHub releases, daily check (30 s after launch + hourly timer), download, installs on quit or when idle (background, 10 min system idle, no bot working); disabled for dev builds and unpublished feed; stops host before install, relaunches in front. | n/a (invisible) | keep |
| D-205 | Host controller | `M/host-controller.ts` `HostController` | Starts/installs/restarts/uninstalls the `kith serve` host, log path, state (running/starting/notInstalled/missingBinary/failed) that drives tray + HostStateView. | n/a (invisible) | keep |
| D-206 | Account / cloud IPC | `M/account.ts`, `M/cloud.ts` | Sign-in sessions (Apple/Google/GitHub) and cloud relay claim/approval used by Settings. | n/a (invisible) | keep |
| D-207 | Kith Screen agent | `M/screen.ts` | Registers the macOS launchd helper (SMAppService) for remote screen; "requires approval" surfaces in tray and Computer access. | n/a (invisible) | keep |
| D-208 | SSH tunnels | `M/ssh.ts`, `M/ssh-command.ts` | Opens ssh tunnels to remote kith-serve, host-key trust, status events consumed by SSH rows. | n/a (invisible) | keep |
| D-209 | On-device speech helper | `M/speech.ts` | Spawns `kith-speech` (Swift SFSpeechRecognizer) and streams partial/final text to dictation and calls. | n/a (invisible) | keep |


Desktop notes from the survey: there are no native OS notifications in the desktop app (only the tray alert icon and the island); every menu except the tray and the app menu is a custom React overlay; the only global shortcuts are Alt+Y / Alt+N while an island approval shows; calls are an overlay inside the thread, not a window.

## Build order

Each step is its own PR. iOS first, then desktop. A screen PR may not merge until every inventory row listed for it is still reachable (same action, same states: empty, loading, error, offline, signed out, update needed).

**0. Tokens** (in progress, `kith-redesign-ios-tokens`): colours, type (Inter/Nunito), radii, spacing, sky and night assets, glass helpers. Rows: I-K2, I-K3, desktop D-14, D-200, D-201, D-203. Living bots (I-K1, D-195 to D-199) are not touched.

**1. Eight components**, each with previews/tests, no screen change yet:

| Component | Preserves rows |
|---|---|
| Bot avatar (no tile; 48/30/24/84 sizes; states working, needs you, idle, asleep) | I-K1, I-A6, D-195 to D-199 |
| Crew row (one per room; agent, team and group shapes; ink count badge; time/state; one-line status) | I-A5, I-A6, I-A19, D-43, D-46 (menu items), D-50 |
| Handoff card ("Where we left off") | I-C12, D-91 |
| Decision card (question buttons, permission Allow/Deny/Always, connection request) | I-C9, I-C10, I-F7, D-87, D-88 (diff), D-90 |
| Transcript turn (user bubble, agent bubble, delivery states, author labels, answered-by-backup, notices) | I-B7, I-C4, I-C7, D-78 to D-82, D-89 |
| Tool row (+ stock sheet with command and output) | I-C11, D-96 |
| Status chips (Live, Catching up, Needs you, Mac unreachable, /urgent; update needed; waiting for approval) | I-A14, I-A15, I-A19, I-C1 (connection text), I-C19, D-28, D-52, D-53, D-55, D-72, D-108 |
| Composer / chat box (+, field, mic, call button that hides while typing; placeholders, queue-while-working, slash commands, @mentions, dictation, attachments, drafts) | I-B1 to I-B7, I-B9, I-H4, D-99 to D-108 |

**2. Screens**, in this order:

1. **Crew home** (iOS I-A1, I-A3 to I-A13, I-A16 to I-A19, I-B8 and I-B9 once the dock exists): top bar (profile picture menu, Crew / Board, new-chat menu), list of rooms, dock. Preserves: pin, mark as read, hide, delete, edit profile/group, refresh, hidden bots in Settings, empty roster, update card, removed-computer state, deep links `kith://bot`, `computers`. Computer folding/ordering/filter moves to Profile > Computers, so I-A4 and I-A12 must land in the Profile menu in the same PR.
2. **Board** (no current screen; new). Must not take any existing feature away. Absorbs decisions waiting on you (entry to I-C10) and Waits; wait for the Board layout and the "Crew shares the dock" decision.
3. **Agent chat and threads** (I-C1 to I-C20, I-B3, I-B2): top bar with picture + name opening the agent sheet, phone and ••• pair, thread chips (task rows), place chip and "Where coder works" sheet, trace/tool rows, handoff, file cards, intro and empty states, new session, call disabled reasons. "Correct this" (I-C21) only after the correction backend is agreed.
   Reserve room for (Planned table, P-1 to P-5): a "Now on <project>" divider between turns, the header project label with a Change correction beside the place chip, a one-line grey source line under replies that used memory, and a thread row that can show a summary instead of the first message.
4. **Agent sheet, Details / Memory / Settings** (I-D1 to I-D12, I-G1, I-G2, I-C3 picker if shared): keep every Details section (About, Cost, Schedules, Projects, Harness, Skills/Tools/Plugins), the Settings form (profile, workspace, activity, plugins), colour sheet, both Memory flavours (hq and plain), routines, template, autosave.
   Reserve room for (Planned table, P-4 to P-7): a Memory tab that can take facet chips (audience, room, projects), a room-notes section for group and team rooms, a threads list with an optional "Summary as the row" switch, and the project label with its correction.
5. **Profile menu and Settings** (I-E1 to I-E14, I-E17, I-H as restyle, I-J5 per Open item 2): account, Computers (pair, access request, revoke, colour, route, voice chat, backups, Link Telegram/Discord), notifications, text size, analytics, licenses, version, start over. Extensions, Computer screen, State tab, usage stay where they are until the designer places them (Open 1 to 4).
6. Outside the app (I-J1 to I-J3, I-J6, I-J7): widgets, Live Activity, notifications, deep links; restyle only, after step 5, and re-point `kith://usage`, `plugins`, `computers`.

**3. Desktop**, after the iOS screens, one PR per screen in the same order, preserving D rows: Crew home (D-39 to D-65, D-1, D-40 to D-42), Agent chat (D-71 to D-108 incl. call D-109 to D-116), Agent sheet (D-117 to D-141), Settings/Profile (D-56, D-160 to D-183), tray and menus (D-7 to D-13, D-27 to D-38) unchanged except labels. Island, Extensions, star prompt, first run and the website demo wait for their Open decisions.

## Planned (designed, not built yet)

Designed and settled in principle by Kai (decisions.md, 2026-10-23, "context layers"), but not in any app and not buildable until the host side exists. The screens above must leave room for them (see "Reserve room for" under Build order steps 3 and 4). Existing rows are linked, not repeated: I-D8, I-D9, D-129 to D-134 (Memory tabs and search), I-C21 (Correct this), I-C3 and D-127 (place chip and folder sheet). Design sources are in the designer's app-style folder: `context5` (rooms carry context), `context6` (flat notes with facets), `context4` (citations, project page); `kith-design` boards `knowledge`, `lifecycle`, `threads-people`.

| ID | Feature | What it does | Design source | Status | Existing rows it extends |
|---|---|---|---|---|---|
| P-1 | Room / group memory | A room is a set of participants. A 1:1 room's notes are the agent's memory with that person; group and team rooms get their own "Room notes". Loaded at thread start with About you and project memory; the rest is found by recall (room threads, room notes, project, agent; never other rooms). | context5, context6 panels 1 to 3, 8 | Settled model (rooms, tiers); screens drawn, not built | I-D8, I-D9, D-129 (Memory tabs gain a room section) |
| P-2 | Per-thread context | One summary per thread, plus the full trajectory (messages, tool calls and results, files touched, decisions, outcome) kept forever and searchable; compaction never deletes it. No digest of earlier threads at thread start; later threads reach earlier ones in the same room through recall. | context6 panels 3 and 5 | Settled | I-C12 and D-91 (handoff card shares the summary) |
| P-3 | Project switching inside a thread | A project move mid-thread is a divider, not a restart: "Now on <project>" (or "Now on no project") between turns with Change. The place chip leads with the project name; the header carries the project label, and a wrong guess is corrected there. This replaces "changing the place restarts the thread" once built. | context5 panel 5, context6 panel 4, decisions.md 2026-10-23 | Settled; host shape in the next paragraph | I-C3, D-127 (place chip, folder sheet), I-C21 (Correct this: "move to another project") |
| P-4 | Thread summaries as rows | Setting "Summary as the row": a thread's row in the room shows its summary instead of the raw first message ("the starting point may just be a hi"). Off by default. | context6 panel 8 | Settled; optional setting | threads list in the agent sheet; Board task rows |
| P-5 | Recall citations | One grey source line under a reply only when memory or recall was used; tap opens the source note, or the trajectory at the cited step, for people in that thread's audience. | context4, context5 panels 3 and 4, context6 panel 5 | Open: needs a protocol rule so agents report which notes a reply used | I-C21 (Correct this sits in the source sheet) |
| P-6 | Memory tab facets | Every note is one item with facets: agent (or crew), audience, room (optional), projects. Filters in the Memory tab by audience, room and project; a note on two projects is one note. | context6 panels 1 and 2, kith-design knowledge | Open: flat model waits on Kai's review | I-D8, I-D9, D-129 to D-134 |
| P-7 | Promotion as retagging | In Tidy, widening or narrowing a note's audience is a retag: same or smaller audience applies with Undo; a bigger one asks the person first. | context6 panel 7, kith-design lifecycle | Settled rule; UI not built | I-D8, I-D9 (tidy runs with Undo) |
| P-8 | People and guests in threads | A guest bot gets only its room's notes and recalls inside its owner's view; a revoked person's threads stay, read-only for the owner. | context6 panel 6, kith-design threads-people | Open: assumptions to confirm (private About-you lines not loaded in groups) | access rows in I-D and D-117 to D-141 |

Host shape already decided for P-3 (kith-context, PR #161): user entries carry `data.projects` (a list of project ids). The divider is a notice entry `{text: "Now on <p1, p2>" | "Now on no project", style: "divider", projectChange: {from, to, by: inferred | person}}`. The first inference adds no divider, only a quiet label. `threadProjects {botId, threadId?} -> {projects, by, pinned}` reads the current projects; `setThreadProjects {botId, threadId?, projects}` is the person's correction.

## Not built until settled

Do not build these yet; they are unsettled in the design.

- **context4**: citations on replies and the per-project page. The thread-context chip (`context.png`) and agent-sheet card (`context2.png`) were both rejected by Kai. The "Thread context" item in DESIGN.md Open stays open.
- **threads3 group and team modes** beyond what DESIGN.md says (team room row, group row, who may join a thread someone else started; the backend still refuses non-owners). The row shapes in DESIGN.md Layout can be built; membership and joining rules wait.
- **Dark mode update**: `mocks/dark.png` is behind the latest rules (bubble colours, night sky, chat strip). Build dark from the DESIGN.md Colors section, not from the board.
- **Whether Crew shares the Board dock fully** (same chat box and call button on both). DESIGN.md says "Crew and Board"; the Board layout itself is "only sketched in phone-app v1" (DESIGN.md Open).
- Branding's preset palette for user colours (15 gradient and plain pairs): colour sheet uses placeholders until confirmed.

## Placed: features that had no home (designer, DESIGN.md draft v2, 2026-10-23)

These features exist today and must survive. Draft v1 did not place them; the designer's v2 does (DESIGN.md sections named below, decisions.md designer lines of 2026-10-23). Until the screen that holds each is rebuilt, the current implementation stays as it is.

1. **State tab** (iOS I-A2, I-J4): widget, Live Activity and Dynamic Island setup galleries. Decided: Settings > "Widgets & Live Activities", unchanged inside; the old State tab goes once the tab bar does.
2. **Usage limits** (iOS I-J5, `kith://usage`; desktop D-170): Decided: Profile menu > Usage (`kith://usage` keeps working); desktop keeps Settings > Usage.
3. **Extensions / marketplace and Credentials** (iOS I-F1 to I-F6, I-F8; desktop D-142 to D-159): Decided: Profile menu > Extensions (marketplace + Credentials; the sheet keeps its own computer switcher; `kith://plugins` keeps working).
4. **Remote Computer screen** (iOS I-I1; chat top bar Computer button I-C1): Decided: three entries: the chat subtitle reads "using the computer" while the agent uses it, ••• > "Show the computer", Profile > Computers > a Mac (and `kith://screen`). The controls become glass pills over the screen, edge to edge on black (DESIGN.md "Computer screen").
5. **Call screen look** (iOS I-H1; desktop D-109 to D-113): Decided: keep layout and behaviour, restyle only: full-bleed sky, large bot (160, no tile) with the mic-level ring, round glass controls, End stays the stock red (DESIGN.md "Call screen").
6. **First run: Welcome, Pairing onboarding, Restoring splash** (iOS I-E15, I-E16, I-A18; desktop D-66, D-67): Decided: restyle on the morning sky with the living crew as the hero; copy, demo, sign-in buttons and pairing steps unchanged; the restoring splash is the Crew loading state (DESIGN.md "First run").
7. **Star prompt** (desktop D-70): Decided: Mac only, today's behaviour in a stock sheet (cube mark, one line, Not now / Star on GitHub); not added to iOS (DESIGN.md "Star prompt (Mac)").
8. **Message reactions** (iOS I-C8; desktop D-80 to D-82): Decided: kept. They are user content, so emoji are allowed here (the no-emoji rule is about icons); your own reaction carries a 1 px ink outline (DESIGN.md "Reactions").
9. **Bot-to-bot conversation view** (iOS I-C15; desktop D-92): Decided: unchanged until group and team modes (threads3) settle.
10. **Manual order of the roster** (iOS I-A3, I-A8; desktop D-45): Decided (Kai's default): Settings > Crew order: Automatic (default: needs you, then working, then recent; pinned rooms always on top in your own drag order; the stored manual order breaks ties) | Manual (today's behaviour exactly: your drag order for everything, synced to the host). The roster never re-sorts while the pointer is on the list (DESIGN.md "Crew order").
11. **Desktop notch island** (desktop D-3, D-184 to D-194): Decided: keep every mode, shortcut and placement. It stays black; bots lose their tiles; the amber approval card becomes the decision card on dark; "Needs you" uses the inverted count badge.
12. **Desktop layout of the design**: Decided (DESIGN.md "Mac"): each column is the phone screen it replaces: sidebar = Crew or Board (with the dock at its bottom, Cmd+F, Cmd+1..9), conversation pane = agent chat, inspector = agent sheet; rail keeps bots only; tray, native menus and the Pair iPhone window unchanged except labels.
13. **Website demo build** (desktop D-25): Decided: it follows the redesign (same renderer, so it changes with each Mac PR; check it in each) and stays always-dark until Mac dark mode lands.
14. **Home search and router** (iOS I-B8) and **Correct this** (I-C21): new features, not in the apps at all. Decided: designed, but the router ("Goes to"), memory search and Correct need backend first; the box filtering the list in place does not and ships with Crew home.

Still open for Kai: night sky is built with night-1 (swapping to night-2 is an asset change); the crew order default is built as Automatic.
