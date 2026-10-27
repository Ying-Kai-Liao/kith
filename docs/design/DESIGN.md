---
name: Kith app
description: Your crew of agents on your phone and Mac. Stock Apple underneath, a thin Kith layer on top.
colors:
  bg: "#FFFFFF"
  ink: "#14161B"
  muted: "#6B7280"
  muted-on-sky: "#4B5160"
  line: "#ECEDF2"
  panel: "#F6F6FA"
  accent: "#6A45F2"
  accent-dark: "#B49CFF"
  attention: "#936000"
  attention-bg: "#FFF1DC"
  positive: "#178357"
  negative: "#C93450"
  night: "#14161B"
typography:
  agent-name:
    fontFamily: "Nunito, SF Pro Rounded, sans-serif"
    fontSize: "16px"
    fontWeight: 800
    lineHeight: 1.2
  top-bar:
    fontFamily: "Inter, SF Pro Text, sans-serif"
    fontSize: "16px"
    fontWeight: 600
  body:
    fontFamily: "Inter, SF Pro Text, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: 1.45
  secondary:
    fontFamily: "Inter, SF Pro Text, sans-serif"
    fontSize: "13px"
    fontWeight: 400
  label:
    fontFamily: "Inter, SF Pro Text, sans-serif"
    fontSize: "11px"
    fontWeight: 700
    letterSpacing: "0.06em"
  code:
    fontFamily: "JetBrains Mono, SF Mono, monospace"
    fontSize: "12px"
    fontWeight: 400
rounded:
  chip: "999px"
  row-tool: "10px"
  card: "18px"
  sheet: "36px"
  glass-pill: "28px"
spacing:
  gutter: "16px"
  row-v: "9px"
  stack: "10px"
components:
  bot-avatar:
    size: "48px"
  bot-avatar-small:
    size: "24px"
  chat-box:
    backgroundColor: "rgba(255,255,255,0.58)"
    textColor: "{colors.ink}"
    rounded: "{rounded.glass-pill}"
    height: "54px"
  top-button:
    backgroundColor: "rgba(255,255,255,0.58)"
    rounded: "{rounded.chip}"
    size: "42px"
  decision-card:
    rounded: "{rounded.card}"
    padding: "12px"
  card-report:
    backgroundColor: "{colors.panel}"
    rounded: "14px"
    padding: "11px 12px"
  card-folded:
    backgroundColor: "{colors.panel}"
    rounded: "{rounded.chip}"
    padding: "6px 11px"
  tool-row:
    backgroundColor: "{colors.panel}"
    rounded: "{rounded.row-tool}"
    height: "44px"
  button-primary:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.bg}"
    rounded: "12px"
    padding: "9px 13px"
  button-secondary:
    backgroundColor: "{colors.bg}"
    textColor: "{colors.ink}"
    rounded: "12px"
    padding: "9px 13px"
  you-tag:
    backgroundColor: "{colors.attention-bg}"
    textColor: "{colors.attention}"
    rounded: "{rounded.chip}"
---

> Source: `~/Workspace/design/kith/app-style/DESIGN.md` (the designer's folder), draft v5, copied 2026-10-27. The designer owns this spec: do not edit it here, changes come from the designer and are re-copied.
> Only change from the source: image names point at `docs/design/mocks/...`; boards added since v2 (`now`, `news`, `ring2`, `ring3`, `replies`, `access`, `morecards`) and `search` ship as `.jpg`, downscaled to 1600 px to keep the repo light.
> Boards named below but not copied (`agent-sheet.png`, `call-dark.png`, `call.png`, `home-v3.png`, `home-v4.png`, `mac-call.png`, `mac-main.png`, `mac-settings.png`, `rooms.png`, `settings.png`) stay in the designer's folder. The sky images are in the apps (iOS `Sky.xcassets`, desktop `renderer/assets/sky`).
> The text below is verbatim, including its mentions of the previous design by its old name.
> Terminal threads (continue anywhere): ~/Workspace/design/kith/app-style/continue-anywhere/spec.md. Feature mapping and build order: `feature-inventory.md`.

# Design System: Kith app

status: draft v5, 2026-10-27 (v5 settles Kai's taste picks: the words agent and bot, Automatic crew order, the night-1 sky, Draft card A; v4 replaces threads with flat chats + handoff and topic cards, the Board sections, news cards and context on tap; v3 added reply cards: the card set, commands, access and keys; v2 added homes for the 14 unplaced features, Mac layout, dark roles), from Kai's decisions (decisions.md, designer lines of 2026-10-23 and 10-24).
Boards: `docs/design/mocks/now.jpg` (rooms, handoff + topic cards, Board), `docs/design/mocks/news.jpg`, `docs/design/mocks/ring2.jpg` + `docs/design/mocks/ring3.jpg` (context), `home-v4.png` (home), `home-v3.png` (routing, call, colour sheet), `docs/design/mocks/system.png` (components), `docs/design/mocks/replies.jpg` + `docs/design/mocks/access.jpg` (reply cards).
Brand source: branding's `brand/` (BRAND.md, brand.tokens.json, living-bots/LAYERS.md). Brand wins where they disagree.

## Overview
Kith is your crew of agents, and the app is where you talk to them. It is **stock iOS 26 underneath** (lists, menus,
sheets, swipe actions, SF Symbols, Liquid Glass) with a **thin Kith layer** on top: the living crew, a morning sky,
a handful of own components, Nunito for agent names. It keeps what works in Codync and adds rather than reacts.

Four ideas carry it:
1. **The bottom is for talking.** Only the chat box lives there; + inside it adds files and images.
2. **One crew, not machines.** Agents run on whichever Mac they live on. Machines appear only when they matter.
3. **A chat is a chat; work shows as cards.** Rooms are flat chats. Work handed to another agent shows as a handoff
   card where it was asked; a smaller purpose becomes a topic card only when you split it off.
4. **The bots are the colour.** The page is white with a real sky at the top; colour comes from the crew.

**Words** (Kai, 2026-10-27): Kith's own crew are **agents** in every piece of UI copy ("New agent", "Share access
to video"). **Bot** means only an outside bot someone brings in (Grok, Muse, a Codex bot): bots are invited, never
made ("Invite a bot", beside "Invite a person"). In this file "bot" also names the drawn face (Bot avatar); that is a
component name, never copy.

## Colors
- Ground: white. On home, a real morning sky (clouds with warm peach edges, `sky/sky-3.png`) fills the top and
  fades to pure white by mid-screen; it stays still while the list scrolls over it. Agent chats keep a short strip of the same sky behind their top bar. Other screens are plain white.
- Ink `#14161B` for text and primary buttons. Muted `#6B7280` (on the sky: `#4B5160`).
- Accent violet `#6A45F2` sparingly: selected states, links. "Needs you" is a small ink count badge (18 pt, white Inter 11 bold, tabular figures: how many questions wait on you) + the question in ink (no amber pill, no `you` tag). The Board tab carries the total (16 pt badge). Green and red
  only for results (tests passed, errors), always with a word.
- Bot colours: each bot has a default pair; the user picks from **presets** (branding's palette), as **gradient or
  plain**. No free colour wheel. The bot body is recoloured through its tint layer; eyes never change. A tinted
  surface (decision card) uses the bot's pair at low strength.
- Dark mode: night `#14161B` ground; the home sky and the chat strip become the same clouds at night (`sky/night-1.png` with faint stars; chat strip `sky/header-night.png`, day `sky/header-day.png`). Bubbles #2C303A (you) / #1E2129 (agent), glass dark. Board: `docs/design/mocks/dark.png` (stale: it still shows the cube, the YOU pill, violet and no call button; the rules in this file win).
  Dark roles come from branding's `brand.dark.tokens.json`: bg night `#14161B`, panel coal `#1E2129`, ink white, muted
  `#B4BAC4`, line slate `#2C303A`, accent `#B49CFF`, positive `#4CC38A`, negative `#FF7A8E`. The needs-you count badge
  inverts (white, night figures). Glass dark = coal at 60% with blur and a 1 px white edge at 8%. Night sky asset:
  `night-1` (Kai kept it, 2026-10-27).

## Typography
- Agent names: Nunito 800, 16. The only place Nunito appears in the app UI.
- Top bar text: Inter 600, 16. **No large titles, no display titles anywhere.**
- Body Inter 15/1.45, secondary 13, labels 11 bold tracked, code and numbers JetBrains Mono 12.
- All text follows Dynamic Type; Inter can fall back to SF Pro.

## Layout
- **Home / Board**: top = your profile picture (left; until a photo is set, a piece of sky: a cloud from the Kith sky over a dawn, clear or dusk tone, seeded per account, no letter), Crew | Board switch (centre), new-chat (right). Middle = crew list.
  Bottom = the chat box (+ for files and images, field, mic) with a round call button to its right, floating glass; while you type the call button slides away and the box takes the full width (back when the text is cleared). No tab bar, no search bar, no title, no machine name.
- **Crew** list: one row per **room**, sorted needs-you → working → recent. A room is any place people talk: your 1:1
  with an agent, a team room (teammates + one default agent), a group (several agents + people, @names route). Your 1:1
  with coder and the room you share with Mia are two rows ("coder with Mia"). Agent row = bot (48, no tile), Nunito name,
  time or state, one sentence of status. Team row = the agent's face with the teammate's sky patch on its lower-left
  (working dots keep lower-right). Group row = two bots on the diagonal + one sky patch, name in Inter 600 so it never
  reads as an agent. A teammate sees an agent's row only when they're a member of it. Long-press: pin, colour, sleep,
  share. Board: `docs/design/mocks/crewteams.png` (A).
- **Crew order**: pinned rooms sit on top in your own drag order (as in Messages), always. Below them the order is
  **Automatic** by default (Kai kept it, 2026-10-27): needs-you, working, recent; inside a group the stored manual order breaks ties. Settings >
  Crew order: Automatic | Manual. Manual keeps today's behaviour exactly (your drag order for everything, synced to the
  host); needs-you still shows by the count badge. Drag and Move up / Move down work on pinned rooms always and on all
  rooms in Manual; in Automatic, dragging an unpinned row onto the pinned area pins it. Never re-sort while a finger or
  pointer is on the list; apply the new order when it is released or the list reappears.
- **Board**: Needs you, Working, Done today. Handoff (and topic) cards sit in the section of their state, each under
  its room's name (Inter 600 13, muted), answerable right there; plain sessions keep their rows. Waits, Left off cards
  and decisions stay below as today. Only rooms you're in; work in other rooms reaches you as a card in the room that
  sent it. Projects as label filters wait for slice 4. Swipe sideways from Crew. Board: `docs/design/mocks/now.jpg` (3).
- **Agent chat**: the home sky shows behind the top bar and fades out by ~170 pt (day: sky-3; night: the night sky). Top = back, bot + name, phone + ••• pair. A flat chat:
  bubbles, tool rows, reply cards, handoff and topic cards in time order. Bottom = the chat box ("Tell coder…").
  The old connection subtitle becomes a status chip under the name when not Live. While the agent uses the computer,
  the subtitle reads "using the computer" with a `display` symbol; tapping it opens the screen view. ••• always has
  "Show the computer" when the agent has computer use.
- Gutter 16, row padding 9 vertical, thumb-zone actions in the bottom half, 44 pt minimum targets.

## Elevation & Depth
- Liquid Glass only for floating controls (top buttons, chat box, menus, sheets): translucent white, blur, a thin
  white edge. Content never sits in glass.
- Bots carry a soft drop shadow (0 2 4, 12% ink) so they sit on white or sky without a tile.
- Menus open with a light blur of the content behind.

## Shapes
Pills for glass controls and chips; 18 pt cards; 10 pt tool rows; 36 pt sheets. Bots have no frame, tile or mask.

## Components
### Bot avatar
The crew bot itself, 48 (list), 30 (top bar), 24 (in a chat), 84 (colour sheet). No background tile. States from
the living-bots motion spec: working (hop dots badge), needs you (looks up), idle, asleep (faded, eyes shut).
### Chat box (home)
Search, router and composer in one. Placeholder "Find or ask…". Empty: mic. Letters: the list above turns into results
in place (one ranked list while typing; scroll drops the keyboard and groups People, Groups, Agents, Topics), ending
with "Search the crew's memory" (slow, Mac-side, never per keystroke) and "Ask the crew". Rows open, the arrow sends:
grey while a match leads, ink once the text will be sent; Return reads "Open" or "Send" to match. Opening a result
lands on the matching message; Back returns to the results. Same on Board. Board: `docs/design/mocks/search.jpg` (A). A sentence: shows "Goes to: coder ·
new session" with two alternatives before sending; `@name` skips routing; `/` opens commands. Send always starts a
**new session**. + at its left attaches files or images (Photos, Camera, Files). The call button beside
the box (home and Board) calls the agent **directly**: the agent the box would route to, or pick one. In an agent chat the phone stays at the top right.
### Top controls
- Profile picture: same default for teammates in chats (agents have faces, people have sky). Stock menu: account
  row, Computers (each Mac and its health; pair, filter and order Macs; Show screen per Mac), Extensions (marketplace +
  Credentials, the sheet keeps its own computer switcher; `kith://plugins`), Usage (`kith://usage`), Settings
  (includes "Widgets & Live Activities", the old State tab galleries, unchanged inside). Desktop adds Support.
- Crew | Board: segmented glass switch.
- New chat (square.and.pencil, as in Messages): native menu: New agent (template or crew), Invite a person, Invite a bot, New group chat.
### Room chat (agent chat and group rooms)
A room is a purpose; its members (people and agents) belong to the room. The chat is flat: your bubble right, agent
bubbles left with the bot (24) and Nunito name, tool rows and cards under the reply that made them. No thread chips,
no per-message level (Codync's thread chip is gone as of v4; `docs/design/mocks/threads2.png` is superseded). Long-press a message:
Reply, **Make a topic from here** ("This and the replies after it"), Copy, Correct this, Compact now (agent replies).
1:1 chats and group rooms work the same. Board: `docs/design/mocks/now.jpg` (1, 2, 4).
### Handoff card
When an agent hands work to another (`kith ask` today shows a Bash row there), one card sits under the message that
caused it and updates in place. Header: the two bots (18) "designer → lab" pair, title (Inter 600 13.5), time or state
on the right. States (`docs/design/mocks/now.jpg` 6, architect model-checks them before the coder brief):
- **Sent:** the brief and its limits as the agent wrote them ("Brief out · $3 cap · waiting for lab to start").
- **Working** (grey, reports): working dots, one live line, the **limits line** (see Context below).
- **Needs you** (tinted in the worker's pair): the question + buttons, answered on the card, in the room or on Board.
  After you answer, the question and your answer stay on the card as one line (white, radius 10) and it goes back to
  working.
- **Done:** the result in a line or two with mono facts, then it folds.
- **Folded:** one pill line like a folded reply card ("Jev accuracy test · 91% right", time).
- **Stopped:** "… · you stopped it", no red: stopping isn't a failure.
Tap opens what exists today (the worker's session); the peek sheet (brief, Stop, Join, box to the worker) is slice 3.
When the worker's result is a reply card, the card becomes the handoff's body.
### Topic card
A topic starts only when you split one off (long-press "Make a topic from here"), or when an agent suggests one and
you tap to accept. Nothing else starts a topic. The run folds into one card, titled with your words (rename allowed):
the two faces, latest line ("designer: Safe enough at 0.85."), "4 messages", time; needs-you rises onto the card
tinted. It opens the same way a handoff does. A topic never sits inside another topic; the room goes on as a chat
below it. Board: `docs/design/mocks/now.jpg` (4, 5).
### Place chip
Folder icon + project · subfolder: where a session runs, picked by the router so the agent loads the right context.
On the route card before sending (with a one-line reason and alternatives), in a handoff or topic card's header,
and in the open session. Tap → "Where coder works" sheet: Suggested (with reasons), Projects, Choose a folder…, Mac. Changing it
starts the session fresh in the new place. Unsure router = decision card with two places. Board: `docs/design/mocks/place.png`.
### Correct
How to tell an agent it got something wrong so it sticks (a reply fixes this session only). Main entry: long-press an
agent reply → "Correct this", prefilled. Also a small grey "Correct" beside the Call capsule in the agent sheet and
"Correct…" in the chat's ••• menu. One field, "What should coder do differently?"; Kith answers with a proposed-fix
card that names where it goes: a memory note (before → after), a learned skill the agent writes itself, or a move to
another project (restarts there). Apply / Edit / Cancel. Every correction lands in Memory history as "You · corrected"
with Undo. Project decisions are append-only: a wrong one gets a correcting line. Builder is for new agents, not fixes.
Board: `docs/design/mocks/context3.png` (B).
### Decision card
An agent's question as buttons, tinted with that agent's colours; permission prompts use the same card (Allow /
Deny / Always). Free text still works in the chat box. Answerable without opening the chat.
### Reply cards
Agents talk in text and use their tools as usual; when the answer is really a plan, a result, a choice, a number, a
form, or something only you can give, the host builds one card from this **closed set** out of what it already knows
and the app draws it natively (KithUI on iPhone and Mac; agents send nothing new and never draw a card, a new card is
a design decision). At most one card per reply. Boards: `docs/design/mocks/replies.jpg`,
`docs/design/mocks/access.jpg`; generators `replies_gen.py`, `access_gen.py`. Kai kept every card on both boards (2026-10-24).
- **One colour rule:** tinted in the agent's pair at low strength (the decision card's tint) = it waits on you; panel
  grey `#F6F6FA` = it is reporting. Inside a card: white groups (radius 12, hairline `#ECEDF2` rows), ink primary
  button, white secondary. Card radius 14, padding 11/12, title Inter 600 13.5.
- **Folds when done:** an answered or finished card shrinks to one pill line in the chat (green check or a thumb,
  what happened, time): "Plan · 5 of 5 done", "You picked Morning sky", "Allowed Notion · library".
- **Text twin:** every card has a plain-text version for Telegram, Discord, Slack and notifications: numbered choices,
  pictures as photos, "reply 1 or 2". Anything the app can't read shows as its twin.
- **Plan** (report): title, "3 of 5" in mono, a 4 pt progress bar in the agent pair, steps updating in place (done =
  check, muted; now = spinner, semibold; next = ring, light). Replaces "done X" messages.
- **Result** (report): what came out with its actions: icon tile, title, mono facts (#172 · 3 files · +84 / −12), a
  green word for checks, one or two buttons (Open, Diff).
- **Options** (asks): two or three choices, a picture each (96 pt), name + one line; "Tap one, or say what to change".
- **Numbers** (report): one mono figure (26), one line of context, at most five ink bars with tabular figures, one row
  to go further. Never a dashboard.
- **Form** (asks): prefilled grouped rows (label 52 pt muted, value, chevron or up-down), Add / Not now.
- **Decision card** (asks): as above; unchanged.
- **Command** (asks): a command only you can run (password, browser sign-in, hardware key). Terminal icon + "Run this
  on your Mac", one line of why, the command in a white mono box with copy, "Mac · folder" line, Open terminal
  (primary) / Copy, "I'll carry on when it finishes." Owner only: a guest sees "Waiting for Kai".
- **Terminal in the card:** Open terminal turns the same card into a live terminal on that Mac (Face ID first on
  iPhone): night `#14161B` box, JetBrains Mono 11.5, the command typed but not run until Return; a key row (esc, tab,
  ctrl, up, down as drawn chevrons, Return in ink); expand icon for long output; the box placeholder reads "Type in
  the terminal…". Note under it: what you type goes to the Mac; the agent sees only the exit code and the last
  lines. On exit it folds to "You ran launchctl kickstart · exit 0". Flat in the chat, never a sheet or a page.
- **Access** (asks): "designer wants Notion" (service tile + name), one line of why, your accounts for that service
  as radio rows (name + whose), "Another account", "Only designer · until you take it back", Allow / Not now.
  Allow = `kith-auth grant <agent> <service> <profile>`.
- **Key** (asks): when the granted account has no key: key icon + "Notion key for library", where to get it, a
  secure field (ink 1.5 pt ring, dots, reveal eye), lock line "Saved to the Keychain on <Mac>. No agent ever reads
  it; <service>'s tool gets it when it runs.", Save key / Where do I find it? Save = `kith-auth set`. The value never
  enters the chat, the transcript or any agent's context. Its text twin says "Open Kith to add this key"; keys are
  never pasted into a chat.
- **News** (report; the Digest card for a routine's brief): built by code from the text the routine already sends
  with `kith-send`, no Jev, so it ships with the code-made cards. A message in the agent's flat chat; header = clock
  + routine name ("Morning brief · Sat 25 Oct", "every day 7:00"); rows = title (Inter 600 13.5), one line, source ·
  domain (muted 12), the whole row opens the link; three shown + "Show N more" (expanded shows all). Folds to one
  line after its day ("Morning brief · Fri · 6 items"), tap opens in place; opened items go quiet. On Board under
  Done today, never Needs you. Telegram keeps the text unchanged. **A rows** first; **B** link pictures (Apple link
  previews fetched by the app: first item wide, others small) after. Board: `docs/design/mocks/news.jpg`.
- **Draft** (asks; Kai picked A, 2026-10-27): a message the agent wrote for you to send. Jev-made, so it waits for
  real drafts and lab's rerun (build step 8). Tinted. Header = service tile + "Draft · Gmail"; a white group laid out
  as the letter itself (To and Subject rows, the body under them); Approve (primary) / Change / Not now inside the
  card, and one fixed line under the buttons: "Approve tells <agent> to send it as shown." No send icon, no service
  logo. Every word comes from the reply; a slot Jev isn't sure of is left out. Approve only sends a message as you
  (your bubble quotes the draft + "Approve"); the agent's own send follows as its tool row, and the card folds to
  "You approved <subject>". Show as text folds it back to the reply in place ("Shown as text · Show as card").
  Text twin: the draft, then "reply 1 to approve, 2 to change". Board: `docs/design/mocks/morecards.jpg` (A; B, answers as reply
  chips above the box, not taken).
- **Approval** (asks): today's permission card (Wants to run a command, mono command box, Mac · folder, Allow once /
  Always allow in <folder> / Deny in red), now tinted like every asking card. "Always allow" becomes a rule on the
  agent's Access tab.
### Tool row
One line per tool call, 44 pt: verb + object, result on the right in mono. Tap: stock sheet with command and output.
No terminal view anywhere, except the Command card's terminal (see Reply cards).
### Left off card
"Where we left off" for a sleeping session: what was done, next step, files/PRs. Only real fields; nothing invented.
Stays on Board below the state sections. (Was "Handoff card" before v4; that name now means the card above.)
### Context
How full an agent's context is, hidden until you ask (Kai, 2026-10-25). Ring: 2 pt ink arc on a 16% ink track, a
grey tick at the agent's compaction point (coder 40%), percent in JetBrains Mono beside it. Data per request from
the host's token log (`cost.db` hq_req: input + cache), so each reply shows the context it was written with.
- **Messages:** tap an agent reply = a line under its bottom-right (time, ring 13, "31% context"); tap again or scroll
  and it goes; the bubble doesn't change size. Long-press = the menu opens with a header: ring (26), "31% context",
  "62k of 200k · compacts at 40%", then Reply, Copy, Correct this, **Compact now**. Board: `docs/design/mocks/ring2.jpg` (A + C).
- **Cards:** a working handoff card carries one **limits line** under a hairline: budget bar + "$0.70 / $3", "12 / 30
  min", ring + "55%"; whichever is closest to its limit is the one to watch. Long-press any card = the same menu
  header, then Open, Compact now, Stop. Topic cards show no ring (no limits); done and folded cards none (the
  session is over). A handoff's ring is the worker's session, not the room's. Board: `docs/design/mocks/ring3.jpg` (D + B).
- Nowhere else: no ring in the top bar, the Crew list or Board rows. Compaction shows as a quiet line in the chat:
  "Compacted at 14:20 · a summary was kept".
### Status chips
Word + colour, never colour alone: Live, Catching up, Needs you, Mac unreachable, /urgent. Machine trouble shows as
one chip ("Mac mini busy, 1 queued"), one tap from Computers.
### Colour sheet
Preview of the bot, Gradient | Plain switch, preset swatches, Reset to default.
### Reactions
User content, so emoji are allowed here (the no-emoji rule is about icons). Chips under the message: panel pill,
emoji + count (Inter 13, tabular). Yours carries a 1 px ink outline, not the accent. The add-reaction control is an
SF Symbol (`face.smiling`). Quick set unchanged. Long-press (iOS), right-click and hover (Mac) as today.
### Call screen
Keep today's layout and behaviour; restyle only. Full-bleed sky (morning by day, night sky in dark mode), the
agent's bot large (160, no tile) with the mic-level ring in its colour pair, name in Nunito, state in Inter, timer and
"Voice by …" in mono 12, captions as plain You / agent lines (not bubbles), notice line in muted. Controls are round
glass buttons with SF Symbols; End is the stock red `phone.down.fill` (the one red that is not a result).
Incoming call (an agent calls you): same sky and bot, "is calling you", one glass line of why + room, Reply in chat /
In 10 min pills, Decline (stock red, left) / Accept (stock green, right); CallKit's system screen when locked. Boards:
`call.png`, `call-dark.png`.
### Agent sheet and Settings
Boards: `agent-sheet.png` / `-dark` (Details, Memory hq + plain, Settings, permission mode = Claude Code's six modes in
a stock menu, colour sheet). Tabs are Details | Memory | Access | Settings: **Access** (`docs/design/mocks/access.jpg`, column 6) lists
Accounts (service · profile, since when, Remove), Keys (name, Keychain · last used, opens its history), Always allowed
(rules from approvals, Remove), Asks before (the permission mode menu moves here from Settings) and People (who may
use this agent, mode, budget). Remove is one tap and maps to `kith-auth revoke` and `settings.png` / `-dark` (account, Computers with Show in Crew, People with guests,
long-press computer menu, Voice chat). Generators share `kit_gen.py` (tokens as CSS variables, light + dark).
### Computer screen
The remote screen view keeps all controls; they become glass pills over the screen, which stays edge to edge on
black. Entries: the chat subtitle while in use, ••• "Show the computer", Profile > Computers > a Mac, `kith://screen`.
### First run
Welcome, pairing and the restoring splash sit on the morning sky with the living crew as the hero. Copy, demo, sign-in
buttons and pairing steps unchanged. The welcome line is the one place text goes above body: Inter 600, 20, a
sentence, centred under the crew. Buttons: primary ink, secondary white. The restoring splash is the same scene as
the Crew loading state.
### Star prompt (Mac)
Today's behaviour (after setup, once more a week later, at most two asks) in a stock sheet: cube mark, one line,
Not now (secondary) / Star on GitHub (primary). Not added to iOS.
### Icons
SF Symbols in the app (phone.fill, square.and.pencil, plus, mic.fill, chevron.down/up, ellipsis, gearshape). Never emoji.

## Mac
- **Each column is the phone screen it replaces.** Sidebar = Crew or Board; conversation pane = agent chat; inspector
  = agent sheet. Same components, same rules.
- **Sidebar** (resizable 260–420, collapses to the rail under 180): top = profile picture (left, under the traffic
  lights), Crew | Board switch, new-chat. A short morning sky at its top fading to white. List = rooms (Crew) or
  Board sections. Bottom = the dock at sidebar width: "Find or ask…" box + call, filtering the list in place as on the
  phone. Cmd+F focuses it; Cmd+1..9 open results; the old palette survives only for the collapsed rail.
- **Conversation pane**: agent chat exactly as on the phone (sky strip behind the top bar, picture + name opens the
  agent sheet, phone + ••• right, the flat chat, "Tell coder…" box at the bottom). No selection = the living crew empty
  state.
- **Inspector**: the agent sheet as the right panel (wide) or sheet (narrow), as today's Details panel.
- **Board** opens a card's session in the conversation pane. Crew/Board also from View menu (⌥⌘1 / ⌥⌘2) and a two-finger swipe
  on the sidebar.
- **Rail**: bots only (no tiles), profile picture at the top, new-chat, Extensions, update badge.
- Tray, native menus, Pair iPhone window: unchanged except labels.
- **Notch island**: keep every mode, shortcut and placement. It stays black (it extends the notch). Bots lose their
  tiles; the amber approval card becomes the decision card on dark (agent pair at low strength, Allow / Deny / Always);
  "Needs you" uses the inverted count badge. Done / stopped notices keep green / red, always with the word.
- **Website demo** (`/demo/desktop/`) follows the redesign: it is the same renderer, so it changes with each Mac PR;
  check it in each. It stays always-dark until Mac dark mode lands.

- Boards: `mac-main.png` (Crew + chat, inspector, rail + sheet, empty, Board, search), `mac-settings.png` (Profile
  menu, Settings as its own 860×600 window with pages), `mac-call.png` (call in the pane, output menu, incoming panel,
  unavailable); each with `-dark`. Generators `mac_kit.py`, `mac_*_gen.py`.
- **Settings** is a window (⌘,), not a sheet: sidebar of pages in iPhone order, plain ground, same cards.
- **Incoming call** on Mac: floating 340×430 panel top right over other apps, Decline / Accept only.

## Do's and Don'ts
### Do:
- Keep the bottom to the chat box (+ attach, field, mic).
- Keep rooms flat chats; show handed-off work as one card that updates in place, fold what's done.
- Let the crew bring the colour; keep chrome white and glass.
- Show machines only when something needs attention or the user asks.
- Reuse stock iOS controls before drawing anything custom; a new style becomes a component first.
### Don't:
- Large or display titles, top search bars, bottom tab bars, a call button that stays visible while typing.
- Tiles or frames behind bots; recolouring the eyes or the cube logo.
- Grain, strong gradients, or the logo gradient as a page background.
- Floating bots or decorative motion on home.
- Emoji or Unicode glyphs as icons.
- Secrets in a chat, a card the agent can read, or a text twin; keys go only through the Key card's secure field.
- Cards an agent composes itself, or more than one card per reply.
- A topic per message, or topics inside topics.
- An always-on context ring or percent on lists, top bars or Board.
- Framing Kith against Codync: keep what works.

## Open
- Reply cards need backend. The host builds every card; agents send nothing new (no card block, no PROTOCOL rule).
  Report cards: code projectors for known tools (routines, message_bot, plan, send_file), then Jev (pick-only model:
  it picks the type and which strings fill each slot, never writes a word) sorting other tool results and finished
  replies into report cards, behind one owner opt-in. Asking cards (Options, Form, Approval, Access, Key, Command)
  come only from structured sources: a permission request, MCP elicitation, AskUserQuestion or the host itself, never
  from Jev. Still needed: a host endpoint for an owner-only terminal with a command typed and not run (today only
  setup steps stream a terminal), and app calls for grant / set / revoke. Apps draw cards natively (SwiftUI on iPhone,
  React on desktop); an unknown type shows the plain tool row. (architect, settled with Kai 2026-10-25)
- Rooms, slices 1-2 (architect's purpose-rooms plan, 2026-10-25): before the build, fix the bugs underneath (the owner
  can't answer cards on his own `kith ask` runs; a fresh terminal takes no writer claim), model-check the handoff
  states, then one coder brief. Later, after Kai tries these (drawn on `rooms.png`, not decided): the peek sheet, the
  room list with labels and New room, the home Goes-to card and "Who should take this?", the Left off strip, a
  routine as a room, Jev cards in rooms.
- News cards need the host to post a routine's `kith-send` into the app chat (today Telegram only).
- Context needs the host to expose per-request tokens and the agent's compaction point to the apps, and a Compact now
  call.
- Bot-to-bot conversation view: unchanged until group and team modes (threads3) settle.
- Home router ("Goes to"), "Search the crew's memory" and Correct need backend first; the box filtering the list in
  place does not and ships with Crew home.
- Session context (what an agent loaded and wrote for a session): context.png chip and context2.png agent-sheet card both rejected by Kai (2026-10-23); new direction needed. Places are named by project (briefcase icon), folder only outside a project.
- Any room member may join a session someone else started (Kai, 2026-10-23); the backend still refuses non-owners.
- Branding: confirm the preset palette for user colours (gradient + plain versions of the 15 pairs).
