import { callModelLabel, callModels, hqSettings, PERMISSION_MODES, permissionLabel } from '@shared/bot-sections'
import { ChoicePicker, ValueRow } from '../../../components/Controls'
import { Text } from '../../../components/Text'
import type { KithInfoStore } from '../../../store/kith-info-store'

/**
 * An hq bot's permission, call model, model, effort and Fast mode, once. hq holds them: the permission is written through to hq and
 * shown as hq reports it, the others hq has no setting for, so they are read-only and say "Set in Kith".
 */
/** The call-model choices, plus the current one when this app doesn't know it. */
function callOptions(kith: KithInfoStore, current: string) {
  const known = callModels(kith.info?.callModelDefault ?? null)
  return known.some((m) => m.id === current) ? known : [...known, { id: current, label: callModelLabel(current) }]
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
  return (
    <>
      {rows.map((row) => (
        <ValueRow key={row.key} label={row.label} detail={row.hint}>
          {row.editable && row.key === 'callModel' ? (
            <ChoicePicker
              selection={row.value}
              options={callOptions(kith, row.value)}
              onChange={(model) => { if (!kith.callModelSaving && model !== row.value) void kith.setCallModel(model) }}
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
      {!inSheet && kith.permissionError ? <Text size="footnote" tone="danger" className="fade-in" role="alert">Permission not changed: {kith.permissionError}</Text> : null}
    </>
  )
}
