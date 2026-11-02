import { useEffect, useRef, useState, type ReactNode } from 'react'
import type { Backend } from '@shared/models'
import { Button, DropdownMenu, IconButton, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { Dialog, SheetPage, useDismiss } from '../../components/Overlay'
import { HStack, Spacer, VStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useStore } from '../../store/context'
import { ComputerBadge } from '../ComputerBadge'
import { AgentSheet, AgentTile, NodeNotice } from './AgentSheet'
import { ComposioSection } from './ComposioViews'
import { CredentialsView } from './CredentialsView'
import { CustomConnectorSheet, InstallConnectorSheet } from './ConnectorSheets'
import { BotPreviewSheet, WhichBotsSheet } from './KithSheets'
import { useKithTools } from './kith-tools'
import { type MarketBot } from '@shared/kith-tools'
import { SetupTerminalView } from './SetupTerminal'
import { SkillSheet, type SkillRef } from './SkillSheet'
import {
  appendPage, emptyPage, errorText, hasHiddenItems, market, needsSignIn, replacePage, revealMore, visible,
  type MarketConnector, type MarketSkill,
} from './market-models'
import { AppLogo, FormSheet, ItemGrid, ItemRow, MarketRow, MarketSection, ScreenHeader, ServiceLogo, SkeletonGrid, SkillGlyph, SlideStack, TileIcon } from './pieces'
import { usePlugins } from './plugins'

const connectorId = (c: MarketConnector) => c.name

/**
 * The Marketplace (Grok Bot's layout): one page of shared bots, engines (the ACP registry), connectors and
 * skills for the bots on one computer (the store in context). A connector or skill installs on that
 * computer and then goes onto a kith agent through the CLI (`kith <agent> connect|skill add`); a shared bot
 * installs with `kith install` after its preview.
 * With more than one computer, the header switches between them (the caller swaps the store).
 */
export function MarketplaceView({ computers, computer, onComputer, appsFirst = false }: { appsFirst?: boolean; computers: { id: string; name: string }[]; computer: string; onComputer: (id: string) => void }) {
  const store = useStore()
  const plugins = usePlugins(store)
  const kith = useKithTools(store)
  const dismiss = useDismiss()
  const [search, setSearch] = useState('')
  /** Bumped on each submitted search, so every section searches again. */
  const [searchToken, setSearchToken] = useState(0)
  const [connectors, setConnectors] = useState(emptyPage<MarketConnector>)
  /** The registry page after the ones shown; null at the end. */
  const [connectorCursor, setConnectorCursor] = useState<string | null>(null)
  const [loadingMore, setLoadingMore] = useState(false)
  const [skills, setSkills] = useState<MarketSkill[]>([])
  const [loadingConnectors, setLoadingConnectors] = useState(true)
  const [loadingSkills, setLoadingSkills] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [installing, setInstalling] = useState<MarketConnector | null>(null)
  const [addingConnector, setAddingConnector] = useState(false)
  const [viewingSkill, setViewingSkill] = useState<SkillRef | null>(null)
  const [agent, setAgent] = useState<Backend | null>(null)
  /** The connector or skill being put on (or taken off) bots. */
  const [placing, setPlacing] = useState<{ kind: 'connector' | 'skill'; name: string; title: string } | null>(null)
  const [sharedBots, setSharedBots] = useState<MarketBot[]>([])
  const [loadingBots, setLoadingBots] = useState(true)
  const [botsError, setBotsError] = useState<string | null>(null)
  const [previewing, setPreviewing] = useState<MarketBot | null>(null)
  const botRequest = useRef(0)
  const [addingNode, setAddingNode] = useState(false)
  /** Agents shown before "Load more engines" (two rows of the grid). */
  const [agentLimit, setAgentLimit] = useState(10)
  const [showInstalled, setShowInstalled] = useState(false)
  const [showCredentials, setShowCredentials] = useState(false)
  const request = useRef(0)
  const connectorQuery = useRef('')

  const query = search.trim()
  const matches = (text: string | null | undefined) => (text ?? '').toLowerCase().includes(query.toLowerCase())
  const allAgents = (store.hello?.backends ?? []).filter((b) => b.available || b.installed === true || b.curated === true)
  const agents = query ? allAgents.filter((b) => matches(b.name)) : allAgents
  const shownSkills = query ? skills.filter((s) => matches(s.name) || matches(s.description)) : skills
  const installedCount = plugins.installedConnectors.length + plugins.installedSkills.length

  const loadConnectors = async (q = query) => {
    const id = ++request.current
    connectorQuery.current = q
    setLoadingConnectors(true)
    setLoadingMore(false)
    try {
      const page = await market.marketConnectors(await store.ready(), q)
      if (request.current !== id) return
      setConnectors(replacePage(page.items, connectorId))
      setConnectorCursor(page.next)
      setError(null)
    } catch (e) {
      if (request.current !== id) return
      setConnectors(emptyPage())
      setConnectorCursor(null)
      setError(errorText(e))
    } finally {
      if (request.current === id) setLoadingConnectors(false)
    }
  }

  const loadBots = async (q = query) => {
    const id = ++botRequest.current
    setLoadingBots(true)
    try {
      const rows = await (await store.ready()).kithMarket(q)
      if (botRequest.current !== id) return
      setSharedBots(rows)
      setBotsError(null)
    } catch (e) {
      if (botRequest.current !== id) return
      setSharedBots([])
      setBotsError(errorText(e))
    } finally {
      if (botRequest.current === id) setLoadingBots(false)
    }
  }

  const loadMoreConnectors = async () => {
    if (loadingMore || loadingConnectors) return
    if (hasHiddenItems(connectors)) {
      setConnectors(revealMore)
      return
    }
    if (!connectorCursor) return
    const id = request.current
    setLoadingMore(true)
    try {
      const page = await market.marketConnectors(await store.ready(), connectorQuery.current, connectorCursor)
      if (request.current !== id) return
      setConnectors((old) => revealMore(appendPage(old, page.items, connectorId)))
      setConnectorCursor(page.next)
      setError(null)
    } catch (e) {
      if (request.current === id) setError(errorText(e))
    } finally {
      if (request.current === id) setLoadingMore(false)
    }
  }

  useEffect(() => {
    void plugins.refresh()
    // Picks up CLIs installed or signed in outside Kith.
    void store.refreshBackends()
    void loadConnectors('')
    void loadBots('')
    void kith.refresh()
    void (async () => {
      try {
        // The catalog is the CLI's (`kith skills available`); a skill goes onto a bot with `kith <bot> skill add`.
        const catalog = await (await store.ready()).kithSkillsAvailable()
        setSkills(catalog.map((c) => ({ source: c.name, name: c.name, description: c.description, installed: false })))
      } catch (e) {
        setError(errorText(e))
      }
      setLoadingSkills(false)
    })()
    return () => void request.current++
    // Mount only, like the view's `.task`s.
  }, [])

  const submit = (q: string) => {
    setSearchToken((t) => t + 1)
    void loadConnectors(q)
    void loadBots(q)
  }

  /** The host's id for an installed registry connector: the name `kith <bot> connect` takes. */
  const installedId = (c: MarketConnector) => plugins.installedConnectors.find((i) => i.registryName === c.name)?.id
  const openConnector = (c: MarketConnector) => {
    const id = installedId(c)
    if (id) setPlacing({ kind: 'connector', name: id, title: c.title })
  }

  /** The names of the bots (not agents) that hold one thing, for a row's subtitle. */
  const heldBy = (agents: readonly string[]) => {
    const names = kith.botsOf(agents).map((b) => b.name)
    return names.length ? `On ${names.slice(0, 3).join(', ')}${names.length > 3 ? ` +${names.length - 3}` : ''}` : null
  }
  const withHolders = (held: string | null, text: string) => (held ? `${held} · ${text}` : text)
  const skillOwners = kith.skillHolders()

  const header = (
    <div className="market-heading">
      {/* Everything here lives on one computer; say which. */}
      <HStack gap={10} shrink>
        <ComputerBadge computer={store.computer} size={32} />
        <VStack shrink className="market-heading-titles">
          <Text size="title" weight="semibold" tone="text">Extensions</Text>
          {computers.length > 1 ? (
            <DropdownMenu
              title="Switch computer"
              className="market-computer-switch"
              items={() => computers.map((c) => ({ title: c.name, selected: c.id === computer, action: () => onComputer(c.id) }))}
            >
              <Text truncate aria-label={`Computer: ${store.hostName}`}>{store.hostName}</Text>
              <Icon name="chevron.down" size={10} weight="semibold" />
            </DropdownMenu>
          ) : (
            <Text size="subheadline" tone="secondary" truncate>{store.hostName}</Text>
          )}
        </VStack>
      </HStack>
      <Spacer />
      <Button kind="secondary" onClick={() => setShowCredentials(true)}><Icon name="lock.shield" size={12} />Credentials</Button>
      {installedCount > 0 ? (
        <button className="press market-installed" onClick={() => setShowInstalled(true)}>
          <HStack as="span" align="stretch" className="market-installed-logos">
            {plugins.installedConnectors.slice(0, 3).map((c) => (
              <HStack as="span" align="stretch" key={c.id}>
                <ServiceLogo name={c.name} registryName={c.registryName} size={26} />
              </HStack>
            ))}
          </HStack>
          <VStack as="span" align="flex-start" className="market-installed-text">
            <Text tone="secondary">{installedCount} installed</Text>
            <Text size="caption" tone="secondary">Connectors & skills</Text>
          </VStack>
          <Icon name="chevron.right" size={10} weight="semibold" color="var(--tertiary)" />
        </button>
      ) : null}
    </div>
  )

  const searchField = (
    <div className="market-search">
      <Icon name="magnifyingglass" size={13} color="var(--secondary)" />
      <input
        value={search}
        placeholder="Search bots, engines, connectors and skills"
        aria-label="Search bots, engines, connectors and skills"
        spellCheck={false}
        onChange={(e) => {
          setSearch(e.target.value)
          setAgentLimit(10)
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.nativeEvent.isComposing) submit(search.trim())
        }}
      />
      {search ? (
        <button
          aria-label="Clear search"
          title="Clear search"
          className="market-search-clear"
          onClick={() => {
            setSearch('')
            setAgentLimit(10)
            submit('')
          }}
        >
          <Icon name="xmark.circle.fill" size={13} />
        </button>
      ) : null}
    </div>
  )

  const centered = (node: ReactNode) => <HStack align="stretch" justify="center">{node}</HStack>

  const page = (
    <div className="market-page">
      <div className="market-scroll">
        <div className="market-page-content">
          {header}
          {searchField}
          {appsFirst ? <ComposioSection query={query} searchToken={searchToken} /> : null}
          <MarketSection title="Bots">
            {loadingBots ? (
              <SkeletonGrid />
            ) : sharedBots.length === 0 ? (
              <Text tone="secondary">{botsError ?? (query ? `No shared bots match “${query}”.` : 'No shared bots found.')}</Text>
            ) : (
              <ItemGrid>
                {sharedBots.slice(0, 12).map((b) => (
                  <MarketRow
                    key={b.repo}
                    title={b.name}
                    subtitle={`${b.stars} ${b.stars === 1 ? 'star' : 'stars'} · ${b.description || b.repo}`}
                    added={b.installed}
                    icon={<TileIcon name="person.crop.circle" />}
                    addLabel="Install"
                    onAdd={() => setPreviewing(b)}
                    onOpen={() => setPreviewing(b)}
                  />
                ))}
              </ItemGrid>
            )}
          </MarketSection>
          {agents.length > 0 ? (
            <MarketSection title={query ? `Engines matching “${query}”` : 'Engines'}>
              {allAgents.some((b) => b.node === 'missing') ? <NodeNotice onAdd={() => setAddingNode(true)} /> : null}
              <div className="agent-grid">
                {agents.slice(0, agentLimit).map((b) => <AgentCard key={b.id} backend={b} onClick={() => setAgent(b)} />)}
              </div>
              {agents.length > agentLimit ? centered(<Button kind="secondary" onClick={() => setAgentLimit((n) => n + 10)}>Load more agents</Button>) : null}
            </MarketSection>
          ) : null}
          <MarketSection title="Connectors">
            {loadingConnectors ? (
              <SkeletonGrid />
            ) : connectors.items.length === 0 ? (
              <Text tone="secondary">{query ? `No connectors match “${query}”.` : "Couldn't load connectors."}</Text>
            ) : (
              <>
                <ItemGrid>
                  {visible(connectors).map((c) => (
                    <MarketRow
                      key={c.name}
                      title={c.title}
                      subtitle={withHolders(heldBy(kith.connectorHolders(installedId(c) ?? c.name)), c.description ?? c.name)}
                      added={c.installed}
                      icon={<ServiceLogo website={c.website} name={c.title} registryName={c.name} />}
                      onAdd={() => setInstalling(c)}
                      onOpen={c.installed ? () => openConnector(c) : undefined}
                    />
                  ))}
                </ItemGrid>
                {hasHiddenItems(connectors) || connectorCursor
                  ? centered(
                      <Button kind="secondary" disabled={loadingMore} onClick={() => void loadMoreConnectors()}>
                        {loadingMore ? 'Loading…' : 'Load more connectors'}
                      </Button>,
                    )
                  : null}
              </>
            )}
          </MarketSection>
          {!appsFirst ? <ComposioSection query={query} searchToken={searchToken} /> : null}
          <MarketSection title="Skills">
            {loadingSkills ? (
              <SkeletonGrid />
            ) : (
              <ItemGrid>
                {shownSkills.map((s) => (
                  <MarketRow
                    key={s.source}
                    title={s.name}
                    subtitle={withHolders(heldBy(skillOwners.get(s.source) ?? []), s.description)}
                    added={(skillOwners.get(s.source) ?? []).length > 0}
                    icon={<SkillGlyph />}
                    onAdd={() => setPlacing({ kind: 'skill', name: s.source, title: s.name })}
                    onOpen={() => setViewingSkill(s)}
                  />
                ))}
              </ItemGrid>
            )}
          </MarketSection>
          <MarketSection title="Make your own">
            <ItemGrid>
              <MarketRow
                title="Custom connector"
                subtitle="Any MCP server: a command or a URL"
                added={false}
                addLabel="New"
                icon={<TileIcon name="point.3.connected.trianglepath.dotted" />}
                onAdd={() => setAddingConnector(true)}
              />
            </ItemGrid>
          </MarketSection>
          {error ? <Text size="footnote" tone="danger">{error}</Text> : null}
          <Text size="footnote" tone="tertiary">
            Connectors come from the official MCP Registry, apps through Composio, skills from Anthropic, bots from GitHub, engines from the ACP registry. Everything installs on {store.hostName}; connectors and skills then go onto the bots you pick.
          </Text>
        </div>
      </div>
      {/* Dismissing slides the sheet away; clearing the caller's state would cut it off. */}
      <div className="market-close">
        <IconButton title="Close" icon="xmark" onClick={dismiss} />
      </div>
    </div>
  )

  return (
    <>
      <SlideStack base={page} top={showCredentials ? <CredentialsView onBack={() => setShowCredentials(false)} /> : showInstalled ? <InstalledView back={() => setShowInstalled(false)} /> : null} />
      <FormSheet open={installing !== null} onClose={() => setInstalling(null)}>
        {installing ? <InstallConnectorSheet key={installing.name} item={installing} done={() => { void loadConnectors(connectorQuery.current); openConnector(installing) }} /> : null}
      </FormSheet>
      <FormSheet open={placing !== null} onClose={() => setPlacing(null)}>
        {placing ? <WhichBotsSheet key={`${placing.kind}:${placing.name}`} kind={placing.kind} name={placing.name} title={placing.title} /> : null}
      </FormSheet>
      <FormSheet open={previewing !== null} onClose={() => setPreviewing(null)}>
        {previewing ? <BotPreviewSheet key={previewing.repo} bot={previewing} done={() => void loadBots(query)} /> : null}
      </FormSheet>
      <FormSheet open={addingConnector} onClose={() => setAddingConnector(false)}>
        <CustomConnectorSheet />
      </FormSheet>
      <FormSheet open={viewingSkill !== null} onClose={() => setViewingSkill(null)}>
        {viewingSkill ? <SkillSheet key={viewingSkill.name} skill={viewingSkill} onAdd={() => { const s = viewingSkill; setViewingSkill(null); if (s.source) setPlacing({ kind: 'skill', name: s.source, title: s.name }) }} /> : null}
      </FormSheet>
      <FormSheet open={addingNode} onClose={() => setAddingNode(false)} width={560} height={560}>
        {addingNode ? <SetupTerminalView backend={null} step="node" back={() => setAddingNode(false)} /> : null}
      </FormSheet>
      <FormSheet open={agent !== null} onClose={() => setAgent(null)} width={560} height={560}>
        {agent ? <AgentSheet key={agent.id} initial={agent} /> : null}
      </FormSheet>
    </>
  )
}

function AgentCard({ backend, onClick }: { backend: Backend; onClick: () => void }) {
  const status = backend.installed !== true && backend.curated === true ? 'Not installed'
    : backend.node === 'missing' ? 'Needs Node.js'
    : backend.installed !== true ? 'Sets up on first use'
    : backend.signedIn === false ? 'Not signed in'
    : 'Installed'
  return (
    <button className="agent-card" title={backend.name} onClick={onClick}>
      <AgentTile registry={backend.registry ?? backend.id} icon={24} tile={40} radius={11} />
      <VStack as="span" align="flex-start" gap={3} shrink>
        <Text size="subheadline" weight="semibold" tone="text" truncate className="agent-card-line">{backend.name}</Text>
        <Text size="caption" tone="secondary" truncate className="agent-card-line">{status}</Text>
      </VStack>
    </button>
  )
}

// MARK: - Installed

function InstalledView({ back }: { back: () => void }) {
  const store = useStore()
  const plugins = usePlugins(store)
  const [removing, setRemoving] = useState<{ id: string; name: string; isSkill: boolean } | null>(null)
  const [signingIn, setSigningIn] = useState<string | null>(null)
  const [viewingSkill, setViewingSkill] = useState<SkillRef | null>(null)
  const empty = plugins.installedConnectors.length === 0 && plugins.installedSkills.length === 0

  const signIn = async (id: string) => {
    setSigningIn(id)
    try {
      await plugins.signIn(id)
    } catch (e) {
      store.setError(errorText(e))
    }
    setSigningIn(null)
  }

  const remove = async (r: { id: string; isSkill: boolean }) => {
    try {
      if (r.isSkill) await plugins.removeSkill(r.id)
      else await plugins.removeConnector(r.id)
    } catch (e) {
      store.setError(errorText(e))
    }
  }

  return (
    <SheetPage>
      <ScreenHeader title="Connectors & skills" onBack={back} />
      <div className="market-installed-body">
        <div className="market-scroll">
          <div className="market-page-content">
            {empty ? <EmptyState icon="shippingbox" title="Nothing installed" detail="Connectors and skills you add show up here." /> : null}
            {plugins.installedConnectors.length ? (
              <MarketSection title="Connectors">
                <ItemGrid>
                  {plugins.installedConnectors.map((c) => (
                    <ItemRow
                      key={c.id}
                      title={c.name}
                      subtitle={needsSignIn(c) ? 'Sign in so bots can use it' : (c.command ?? c.url ?? c.description)}
                      icon={c.kind === 'composio' ? <AppLogo url={c.logo} name={c.name} /> : <ServiceLogo name={c.name} registryName={c.registryName} />}
                      trailing={(
                        <>
                          {signingIn === c.id ? <Spinner /> : needsSignIn(c) ? <Button onClick={() => void signIn(c.id)}>Sign in</Button> : null}
                          <RemoveButton title={c.name} onClick={() => setRemoving({ id: c.id, name: c.name, isSkill: false })} />
                        </>
                      )}
                    />
                  ))}
                </ItemGrid>
              </MarketSection>
            ) : null}
            {plugins.installedSkills.length ? (
              <MarketSection title="Skills">
                <ItemGrid>
                  {plugins.installedSkills.map((s) => (
                    <ItemRow
                      key={s.id}
                      title={s.name}
                      subtitle={s.description}
                      icon={<SkillGlyph />}
                      onOpen={() => setViewingSkill({ id: s.id, name: s.name, description: s.description })}
                      trailing={<RemoveButton title={s.name} onClick={() => setRemoving({ id: s.id, name: s.name, isSkill: true })} />}
                    />
                  ))}
                </ItemGrid>
              </MarketSection>
            ) : null}
          </div>
        </div>
      </div>
      <Dialog
        open={removing !== null}
        title={`Remove ${removing?.name ?? ''}?`}
        message={removing ? (removing.isSkill ? 'Bots stop using this skill.' : 'Bots lose this connector, and the keys saved for it are deleted.') : null}
        actions={removing ? [{ title: 'Remove', destructive: true, action: () => void remove(removing) }] : []}
        onClose={() => setRemoving(null)}
      />
      <FormSheet open={viewingSkill !== null} onClose={() => setViewingSkill(null)}>
        {viewingSkill ? <SkillSheet key={viewingSkill.name} skill={viewingSkill} /> : null}
      </FormSheet>
    </SheetPage>
  )
}

/** A quiet "nothing here" message (replaces `ContentUnavailableView`). */
function EmptyState({ icon, title, detail }: { icon: string; title: string; detail: string }) {
  return (
    <VStack align="center" gap={8} className="market-empty">
      <Icon name={icon} size={28} color="var(--tertiary)" />
      <Text size="compactBody" weight="semibold" tone="text">{title}</Text>
      <Text size="compactSecondary" tone="secondary">{detail}</Text>
    </VStack>
  )
}

function RemoveButton({ title, onClick }: { title: string; onClick: () => void }) {
  return (
    <button
      title="Remove"
      aria-label={`Remove ${title}`}
      className="market-remove"
      onClick={(e) => {
        e.stopPropagation()
        onClick()
      }}
    >
      <Icon name="trash" size={13} />
    </button>
  )
}
