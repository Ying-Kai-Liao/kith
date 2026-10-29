// Dev-only component gallery (`npm run components`): every Kith component and state, in the scheme the system is
// set to. Not part of the app build. `?narrow` squeezes the layout to the 260px sidebar.
import { useState, type ReactNode } from 'react'
import { createRoot } from 'react-dom/client'
import '../styles/theme.css'
import {
  BotAvatar,
  ChatDock,
  CountBadge,
  CrewBoardSwitch,
  CrewRow,
  DecisionCard,
  GlassButton,
  HandoffCard,
  NewChatButton,
  OriginTag,
  ProfileButton,
  SkyPatch,
  StatusChip,
  TakeOverDialog,
  ToolRow,
  TopBarTitle,
  TranscriptDivider,
  TranscriptTurn,
  WriterBanner,
  WriterChip,
  pairOf,
  type CrewBoard,
  type CrewFace,
} from '../components/kith'

const face = (name: string, color: string): CrewFace => ({ face: { kind: 'crew', name }, color: `kith:${color}`, id: name })
const coder = face('coder', 'sky')
const designer = face('designer', 'apricot')
const house = face('house', 'blue')
const builder = face('builder', 'orchid')
const finance = face('finance', 'green')
const image = face('image', 'pink')
const coderPair = pairOf('kith:sky')
const designerPair = pairOf('kith:apricot')

const wrap = { padding: '0 var(--kith-gutter)', marginBottom: 32 }
const h = { color: 'var(--kith-muted)', margin: '0 0 12px' }
const col = (w: number) => ({ width: w, maxWidth: '100%' })

function Section({ title, children, note }: { title: string; children: ReactNode; note?: string }) {
  return (
    <div style={wrap}>
      <div className="kith-text-label" style={h}>{title.toUpperCase()}</div>
      {note ? <div className="kith-text-secondary" style={{ color: 'var(--kith-muted)', marginBottom: 10 }}>{note}</div> : null}
      {children}
    </div>
  )
}
const Row = ({ children, gap = 16 }: { children: ReactNode; gap?: number }) => <div style={{ display: 'flex', flexWrap: 'wrap', gap, alignItems: 'flex-start' }}>{children}</div>
const Cap = ({ children }: { children: ReactNode }) => <div className="kith-text-secondary" style={{ color: 'var(--kith-muted)', marginTop: 4, textAlign: 'center' }}>{children}</div>

function Gallery() {
  const narrow = new URLSearchParams(location.search).has('narrow')
  const [tab, setTab] = useState<CrewBoard>('crew')
  const [open, setOpen] = useState(true)
  const [text, setText] = useState('')
  const [typed, setTyped] = useState('Merge it once Greptile is happy')
  const [confirm, setConfirm] = useState(false)
  const [taking, setTaking] = useState(false)
  const thread = (over: object = {}) => ({ replies: 2, time: '8:14', open: false, agent: coder, pair: coderPair, place: { kind: 'project' as const, name: 'kith' }, onToggle: () => {}, ...over })
  const menu = [
    { title: 'Attach files', icon: 'doc', action: () => {} },
    { title: 'Attach photos', icon: 'photo', action: () => {} },
  ]
  return (
    <div style={{ height: '100%', overflowY: 'auto', background: 'var(--kith-ground)', color: 'var(--kith-ink)', ...(narrow ? { width: 260 } : {}) }}>
      <div style={{ position: 'relative', overflow: 'hidden', padding: '28px var(--kith-gutter) 24px', marginBottom: 28 }}>
        <div className="kith-sky-home" />
        <div style={{ position: 'relative' }}>
          <div className="kith-text-agent-name" style={{ fontSize: 28 }}>Kith components</div>
          <div className="kith-text-secondary" style={{ color: 'var(--kith-muted-on-sky)', margin: '4px 0 18px' }}>top bar and dock over the sky</div>
          <Row gap={10}>
            <ProfileButton><SkyPatch id="me" you size={42} /></ProfileButton>
            <CrewBoardSwitch value={tab} onChange={setTab} boardCount={3} />
            <NewChatButton />
          </Row>
          <div style={{ marginTop: 14 }}><TopBarTitle>Top bar text, Inter 600 16</TopBarTitle></div>
          <div style={{ marginTop: 18 }}>
            <ChatDock value={text} onChange={setText} onSend={() => setText('')} onMic={() => {}} onCall={() => {}} attachMenu={menu} />
          </div>
        </div>
      </div>

      <Section title="BotAvatar" note="No tile, bot shadow. Sizes 48 / 30 / 24 / 84; states idle, working, needs you, asleep.">
        <Row gap={24}>
          {(['idle', 'working', 'needsYou', 'asleep'] as const).map((s) => (
            <div key={s}><BotAvatar face={coder.face} color={coder.color} id="coder" state={s} count={2} /><Cap>{s}</Cap></div>
          ))}
          <div><BotAvatar face={coder.face} id="coder-old" color="blue" /><Cap>old colour</Cap></div>
        </Row>
        <Row gap={24}>
          {([84, 48, 30, 24] as const).map((s) => (
            <div key={s} style={{ marginTop: 16 }}><BotAvatar face={designer.face} color={designer.color} id="designer" size={s} state={s === 24 ? 'working' : 'idle'} /><Cap>{s}</Cap></div>
          ))}
        </Row>
      </Section>

      <Section title="SkyPatch" note="Default profile picture: a crop of the sky; seeded by account id; you = dawn.">
        <Row gap={14}>
          <div><SkyPatch id="me" you size={64} /><Cap>you</Cap></div>
          {['acct_mia', 'acct_ren', 'acct_jo', 'acct_sam', 'acct_li'].map((id) => <div key={id}><SkyPatch id={id} size={64} /><Cap>{id.slice(5)}</Cap></div>)}
          <div><SkyPatch id="acct_mia" size={30} /><Cap>30</Cap></div>
          <div><SkyPatch id="acct_mia" size={16} /><Cap>16</Cap></div>
        </Row>
      </Section>

      <Section title="CrewRow" note="Agent, team, group; hover and selected. Shown at sidebar width (260) and wide.">
        <Row gap={24}>
          <div style={col(260)}>
            <CrewRow shape="agent" bot={designer} name="designer" status="Which direction for the app?" right="9:41" count={1} />
            <CrewRow shape="group" bots={[designer, coder]} personId="acct_mia" name="Launch" status="designer: Pick the tab icon set" count={2} />
            <CrewRow shape="team" bot={coder} mateId="acct_mia" mateName="Mia" name="coder" status="Relay fix · running tests" right="working" state="working" selected />
            <CrewRow shape="agent" bot={house} name="house" status="Milk expires tomorrow, added to the list" right="9:02" />
            <CrewRow shape="agent" bot={builder} name="builder" status="Left off: run kith doctor next" right="asleep · 2 h" state="asleep" />
            <CrewRow shape="agent" bot={face('resource-manager', 'teal')} name="resource-manager-with-a-very-long-name" status="Watching Mac mini · swap 2.1 GB, a very long line that has to ellipsise" right="Yesterday" count={120} />
          </div>
          <div style={col(420)}>
            <CrewRow shape="agent" bot={coder} name="coder" status="Editing kith-sessions.py · phase 6" right="working" state="working" />
            <CrewRow shape="agent" bot={finance} name="finance" status="October summary sent" right="Yesterday" />
            <CrewRow shape="agent" bot={image} name="image" status="3 OG cards for the blog" right="Tuesday" />
          </div>
        </Row>
      </Section>

      <Section title="HandoffCard" note="Where we left off: only the fields that exist.">
        <Row gap={24}>
          <div style={col(340)}><HandoffCard fields={{ leftOff: 'Moved the relay reconnect into the sleep hook.', next: 'Run kith doctor, then open the PR.', waitingOn: 'Greptile review', place: 'kith · serve/host', when: 'Yesterday 17:40' }} onOpen={() => {}} /></div>
          <div style={col(260)}><HandoffCard fields={{ leftOff: 'Drafted the three OG cards.' }} /></div>
        </Row>
      </Section>

      <Section title="DecisionCard" note="Tinted in the agent's pair while it waits, grey once answered; 1 to 4 options; permission (Allow once / Always allow in folder / red Deny); guest prompt; answered.">
        <Row gap={24}>
          <div style={col(340)}><DecisionCard kind="question" pair={designerPair} title="Which direction for the app?" options={[{ id: 'a', label: 'Sky and glass' }, { id: 'b', label: 'Flat panels' }]} onChoose={() => {}} /></div>
          <div style={col(340)}><DecisionCard kind="question" pair={coderPair} title="Open a PR now, or batch it with the sleep notice fix?" options={[{ id: 'a', label: 'Open a PR now' }, { id: 'b', label: 'Batch it' }, { id: 'c', label: 'A very long option label that wraps onto a second line' }, { id: 'd', label: 'Ask me later' }]} onChoose={() => {}} pending="b" /></div>
          <div style={col(260)}><DecisionCard kind="question" pair={coderPair} title="Keep going?" options={[{ id: 'a', label: 'Yes' }]} onChoose={() => {}} /></div>
          <div style={col(340)}><DecisionCard kind="permission" pair={coderPair} title="Wants to run a command" subject="npm run typecheck && git push origin flow/kith-redesign" place="Runs on Mac mini · kith" allowLabel="Allow once" alwaysLabel="Always allow in kith" onAllow={() => {}} onDeny={() => {}} onAlways={() => {}} /></div>
          <div style={col(340)}><DecisionCard kind="permission" pair={coderPair} title="Wants to run a command" subject="rm -rf build" place="Runs on Mac mini · kith" waiting="Kai" onAllow={() => {}} onDeny={() => {}} /></div>
          <div style={col(340)}><DecisionCard kind="permission" pair={coderPair} title="Wants to change files" subject="relay/socket.ts" outcome="Allowed once" onAllow={() => {}} onDeny={() => {}} /></div>
        </Row>
      </Section>

      <Section title="TranscriptTurn" note="User bubble right, agent bubble left; thread chip: folded, new, open, your turn; place; origin; writer.">
        <div style={{ ...col(460), display: 'flex', flexDirection: 'column', gap: 12 }}>
          <TranscriptDivider>Today 8:12</TranscriptDivider>
          <TranscriptTurn role="user" thread={thread({ origin: 'terminal' })}>Why is swap so high on the mini?</TranscriptTurn>
          <TranscriptTurn role="user" thread={thread({ replies: 5, newCount: 1, time: '8:52', place: { kind: 'folder', name: 'apps/ios' }, agent: designer, pair: designerPair })}>Add the waits section to Board</TranscriptTurn>
          <TranscriptTurn role="user" thread={thread({ yourTurn: true, replies: 1, time: '9:10' })}>Pick the icon set</TranscriptTurn>
          <TranscriptTurn role="user" thread={thread({ writer: { working: true }, replies: 2, time: 'now', open: open, onToggle: () => setOpen(!open) })}
            replies={<>
              <TranscriptTurn role="agent">Adding the flag. Running the CLI tests first.</TranscriptTurn>
              <ToolRow verb="Run tests" object="cli" result="running" state="running" />
            </>}>Add --resume to the kith CLI</TranscriptTurn>
          <TranscriptTurn role="user" thread={thread({ origin: 'discord', writer: { working: false }, place: { kind: 'project', name: 'a-rather-long-project-name-that-ellipsises' } })}>Ship the sleep notice fix</TranscriptTurn>
          <TranscriptTurn role="agent">Fixed and tested. The reconnect now takes under a second.</TranscriptTurn>
        </div>
      </Section>

      <Section title="ToolRow" note="44px, radius 10. Click opens the sheet with command and output.">
        <div style={{ ...col(360), display: 'flex', flexDirection: 'column', gap: 8 }}>
          <ToolRow verb="Edited" object="relay/socket.ts" result="+12 -3" detail={{ command: 'git diff relay/socket.ts', output: '@@ -10,3 +10,12 @@\n+  refreshToken()' }} />
          <ToolRow verb="Run tests" object="relay" result="passed" />
          <ToolRow verb="Run tests" object="cli" result="running" state="running" />
          <ToolRow verb="Run tests" object="serve/host with a very long object name that will not fit" result="failed" state="error" detail={{ command: 'npm test', output: 'AssertionError: expected 1 to equal 2' }} />
        </div>
      </Section>

      <Section title="StatusChip, CountBadge">
        <Row gap={8}>
          <StatusChip kind="live" /><StatusChip kind="catching-up" /><StatusChip kind="needs-you" /><StatusChip kind="mac-unreachable" /><StatusChip kind="urgent" /><StatusChip kind="machine" detail="Mac mini busy, 1 queued" />
        </Row>
        <Row gap={12}>
          <div style={{ marginTop: 14 }}><CountBadge count={1} /><Cap>1</Cap></div>
          <div style={{ marginTop: 14 }}><CountBadge count={42} /><Cap>42</Cap></div>
          <div style={{ marginTop: 14 }}><CountBadge count={100} /><Cap>99+</Cap></div>
          <div style={{ marginTop: 14 }}><CountBadge count={3} size={16} /><Cap>board 16</Cap></div>
          <div style={{ marginTop: 14 }}><CountBadge count={0} /><Cap>0 hidden</Cap></div>
          <div style={{ marginTop: 14, padding: 10, background: '#000', borderRadius: 10 }}><CountBadge count={2} inverted /><Cap>inverted (island)</Cap></div>
        </Row>
      </Section>

      <Section title="Terminal thread" note="OriginTag, StatusChip writer variant, dividers.">
        <Row gap={10}>
          <OriginTag origin="terminal" /><OriginTag origin="discord" /><OriginTag origin="telegram" /><OriginTag origin="slack" />
          <WriterChip pair={coderPair} /><WriterChip working pair={coderPair} /><WriterChip pair={coderPair} mac="Mac mini" /><WriterChip working pair={coderPair} mac="Mac mini" />
        </Row>
        <div style={{ ...col(440), display: 'flex', flexDirection: 'column', gap: 10, marginTop: 16 }}>
          <TranscriptDivider icon="terminal">Started in the terminal on Mac mini · 9:40</TranscriptDivider>
          <TranscriptDivider>You took over from the terminal · 10:02</TranscriptDivider>
          <TranscriptDivider icon="terminal">Opened in terminal on Mac mini · 10:31</TranscriptDivider>
          <TranscriptDivider>Terminal closed · 10:48</TranscriptDivider>
        </div>
      </Section>

      <Section title="WriterBanner" note="Replaces the dock for a held thread: idle, working, taking over, failed, Mac unreachable, someone else's terminal.">
        <div style={{ ...col(420), display: 'flex', flexDirection: 'column', gap: 10, background: 'var(--kith-panel)', padding: 12, borderRadius: 'var(--kith-radius-card)' }}>
          <WriterBanner phase="idle" mac="Mac mini" pair={coderPair} onTakeOver={() => setConfirm(true)} />
          <WriterBanner phase="working" mac="Mac mini" pair={coderPair} />
          <WriterBanner phase="taking" mac="Mac mini" pair={coderPair} />
          <WriterBanner phase="failed" mac="Mac mini" pair={coderPair} />
          <WriterBanner phase="unreachable" mac="Mac mini" pair={coderPair} />
          <WriterBanner phase="idle" mac="Mac mini" pair={coderPair} mine={false} person="Mia" />
        </div>
        <TakeOverDialog open={confirm} mac="Mac mini" agent="coder" resumeId="4f2a" pending={taking} onCancel={() => setConfirm(false)} onConfirm={() => { setTaking(true); setTimeout(() => { setTaking(false); setConfirm(false) }, 600) }} />
      </Section>

      <Section title="ChatDock" note="Empty (mic + call), typing (send, call folded), attachment, narrow. Type in the first one.">
        <div style={{ background: 'var(--kith-panel)', padding: 14, borderRadius: 'var(--kith-radius-card)', display: 'flex', flexDirection: 'column', gap: 18 }}>
          <div style={col(420)}><ChatDock value={text} onChange={setText} onSend={() => setText('')} onMic={() => {}} onCall={() => {}} attachMenu={menu} placeholder="Tell coder…" /></div>
          <div style={col(420)}><ChatDock value={typed} onChange={setTyped} onSend={() => {}} onCall={() => {}} attachMenu={menu} /></div>
          <div style={col(420)}><ChatDock value="" onChange={() => {}} onSend={() => {}} onCall={() => {}} attachMenu={menu} hasAttachments attachments={<StatusChip kind="live" detail="x" />} /></div>
          <div style={col(260)}><ChatDock value="" onChange={() => {}} onSend={() => {}} onMic={() => {}} onCall={() => {}} attachMenu={menu} /></div>
          <div style={col(420)}><ChatDock value="" onChange={() => {}} onSend={() => {}} showsCall={false} onMic={() => {}} /></div>
        </div>
      </Section>

      <Section title="Top bar pieces">
        <Row gap={10}>
          <ProfileButton><SkyPatch id="acct_mia" size={42} /></ProfileButton>
          <CrewBoardSwitch value="board" onChange={() => {}} boardCount={120} />
          <GlassButton label="More" icon="ellipsis" />
          <NewChatButton />
        </Row>
      </Section>
    </div>
  )
}

createRoot(document.getElementById('root')!).render(<Gallery />)
