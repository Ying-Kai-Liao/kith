import { callModelLabel, callModels, EFFORT_LEVELS, HARNESSES, hqSettings, PERMISSION_MODES, permissionLabel } from '@shared/bot-sections'
import { Button, ChoicePicker, ValueRow } from '../../../components/Controls'
import { Text } from '../../../components/Text'
import type { KithInfoStore } from '../../../store/kith-info-store'
import { useAgentModels } from '../agent-models'

/**
 * An hq bot's permission, call model, harness, model, effort and Fast mode, once. Kith holds them: each is written through
 * (`kith <agent> set …`) and shown as kith reports it; Fast mode has no setting, so it is read-only and says "Set in Kith".
 */
/** The call-model choices, plus the current one when this app doesn't know it. */
function callOptions(kith: KithInfoStore, current: string) {
  const known = callModels(kith.info?.callModelDefault ?? null)
  return known.some((m) => m.id === current) ? known : [...known, { id: current, label: callModelLabel(current) }]
}

/** The choices of a model/effort/harness row, plus the current value when the list doesn't know it. */
function withCurrent(options: { id: string; label: string }[], current: string) {
  return options.some((o) => o.id === current) ? options : [...options, { id: current, label: current }]
}

/** The agent's models on its harness (what the app already lists for it), plus `default`. */
function modelOptions(models: { id: string; name: string }[] | undefined, current: string) {
  return withCurrent([{ id: 'default', label: "Agent's default" }, ...(models ?? []).map((m) => ({ id: m.id, label: m.name || m.id }))], current)
}

/** The permission mode menu (Claude Code's six modes) with its error line: the agent sheet's Access tab, under "Asks before". */
export function PermissionRow({ kith }: { kith: KithInfoStore }) {
  const row = hqSettings(kith.info).find((r) => r.key === 'permission')!
  return (
    <>
      <ValueRow label="Mode" detail={row.hint}>
        <ChoicePicker
          selection={row.value}
          options={PERMISSION_MODES.some((m) => m.id === row.value) ? PERMISSION_MODES : [...PERMISSION_MODES, { id: row.value, label: permissionLabel(row.value), detail: '' }]}
          onChange={(mode) => { if (!kith.permissionSaving && mode !== row.value) void kith.setPermission(mode) }}
        />
      </ValueRow>
      {kith.permissionError ? <Text size="footnote" tone="danger" className="fade-in" role="alert">Permission not changed: {kith.permissionError}</Text> : null}
    </>
  )
}

/** `inSheet`: the agent sheet shows the permission on its Access tab, so it is left out here. */
export function HqSettings({ kith, inSheet = false }: { kith: KithInfoStore; inSheet?: boolean }) {
  const rows = hqSettings(kith.info).filter((r) => !(inSheet && r.key === 'permission'))
  const catalog = useAgentModels(kith.info?.harness ?? 'claude').catalog
  const saving = kith.settingSaving
  // Kith's answer is the only source of these rows: when it is missing, say why and offer Retry, never host-native pickers.
  const unavailable = kith.info ? null : kith.unsupported ? 'this host predates the Kith details call; update the host' : kith.error
  return (
    <>
      {unavailable ? (
        <div className="kith-banner" role="alert">
          <span>Kith settings unavailable: {unavailable}</span>
          <Button kind="secondary" disabled={kith.loading} onClick={() => void kith.load(true)}>{kith.loading ? 'Retrying…' : 'Retry'}</Button>
        </div>
      ) : null}
      {rows.map((row) => (
        <ValueRow key={row.key} label={row.label} detail={row.hint}>
          {row.editable && row.key === 'callModel' ? (
            <ChoicePicker
              selection={row.value}
              options={callOptions(kith, row.value)}
              onChange={(model) => { if (!kith.callModelSaving && model !== row.value) void kith.setCallModel(model) }}
            />
          ) : row.editable && row.key === 'harness' ? (
            <ChoicePicker
              selection={row.value}
              options={withCurrent(HARNESSES, row.value)}
              onChange={(harness) => { if (!saving && harness !== row.value) void kith.setSetting('harness', harness) }}
            />
          ) : row.editable && row.key === 'model' ? (
            <ChoicePicker
              selection={row.value}
              options={modelOptions(catalog?.models, row.value)}
              onChange={(model) => { if (!saving && model !== row.value) void kith.setSetting('model', model) }}
            />
          ) : row.editable && row.key === 'effort' ? (
            <ChoicePicker
              selection={row.value}
              options={withCurrent(EFFORT_LEVELS, row.value)}
              onChange={(effort) => { if (!saving && effort !== row.value) void kith.setSetting('effort', effort) }}
            />
          ) : row.editable ? (
            <ChoicePicker
              selection={row.value}
              options={PERMISSION_MODES.some((m) => m.id === row.value) ? PERMISSION_MODES : [...PERMISSION_MODES, { id: row.value, label: permissionLabel(row.value), detail: '' }]}
              onChange={(mode) => { if (!kith.permissionSaving && mode !== row.value) void kith.setPermission(mode) }}
            />
          ) : (
            <Text tone="secondary" truncate>{row.value}</Text>
          )}
        </ValueRow>
      ))}
      {kith.callModelError ? <Text size="footnote" tone="danger" className="fade-in" role="alert">Call model not changed: {kith.callModelError}</Text> : null}
      {kith.settingError ? <Text size="footnote" tone="danger" className="fade-in" role="alert">{kith.settingError.setting[0].toUpperCase() + kith.settingError.setting.slice(1)} not changed: {kith.settingError.text}</Text> : null}
      {!inSheet && kith.permissionError ? <Text size="footnote" tone="danger" className="fade-in" role="alert">Permission not changed: {kith.permissionError}</Text> : null}
    </>
  )
}
