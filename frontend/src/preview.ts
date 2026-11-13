import { applyTheme, createTheme } from '@codex-proxy/ui/theme'
import type { Account, Bridge, HostAccount, HostSelection, Onboarding, Snapshot } from './api'

export function installPreview(): boolean {
  if (window.codexProxyPlugin || !import.meta.env.DEV) return false
  // 独立开发预览只使用内存夹具，不读取凭据或连接真实 BPS
  const params = new URLSearchParams(location.search)
  applyTheme(document.documentElement, createTheme({ scheme: params.get('theme') === 'dark' ? 'dark' : 'light' }))
  document.body.style.backgroundColor = 'var(--z-color-bg-layout)'
  let accounts: Account[] = params.has('populated') ? [{ id: 'preview-account', label: '个人账号', models: ['gpt-6-astra'], auth: { configured: true, valid: true, status: 'authorized', expiresAt: Date.now() / 1000 + 3600, message: '模拟授权' } }] : []
  let onboarding: Onboarding | null = null, polls = 0, stale = false
  const nativeAccounts: HostAccount[] = [
    { id: 'host-existing', label: '宿主已有账号', email: 'preview@example.test', enabled: true, state: 'ready', groupIds: ['group-a'] },
    { id: 'overlap', label: '重叠账号', email: null, enabled: true, state: 'ready', groupIds: ['group-a', 'group-b'] },
    { id: 'b1', label: 'B1 账号', email: null, enabled: true, state: 'ready', groupIds: ['group-b'] },
    { id: 'b2', label: 'B2 账号', email: null, enabled: true, state: 'ready', groupIds: ['group-b'] },
    { id: 'ungrouped-email', label: '', email: 'ungrouped@example.test', enabled: true, state: 'ready', groupIds: [] },
    { id: 'ungrouped-id', label: '', email: null, enabled: true, state: 'ready', groupIds: [] },
  ]
  const hostGroups = [{ id: 'group-a', name: '分组 A', enabled: true }, { id: 'group-b', name: '分组 B', enabled: true }]
  let hostSelection: HostSelection = { groupIds: [], accountIds: [] }
  const prompt = () => ({ authorizationUrl: 'https://auth.openai.com/api/accounts/deviceauth/authorize?client_id=preview', userCode: 'DEMO-2486', interval: 1, expiresAt: Date.now() / 1000 + 900 })
  const answer = (data: unknown, status = 200) => ({ status, contentType: 'application/json', body: new TextEncoder().encode(JSON.stringify(data)).buffer })
  const snapshot = (): Snapshot => ({ accounts, hostAccounts: nativeAccounts, hostGroups, hostSelection, defaultModel: 'gpt-6-astra', onboarding })
  const published = (status: string, changed = true) => {
    stale = params.has('revision') && changed
    return answer({ status, reloadRequired: stale, snapshot: snapshot() })
  }
  const bridge: Bridge = {
    version: 2,
    async openExternal() { /* 模拟宿主确认，不打开真实授权页面 */ },
    async request(input) {
      await new Promise(resolve => setTimeout(resolve, 80))
      if (stale) { console.error('Stale management target requested'); return answer({ error: { message: '插件页面版本已变更，正在刷新' } }, 409) }
      const body = input.body ? JSON.parse(input.body) : {}
      if (params.has('error')) return answer({ error: { message: '模拟网络错误，请检查宿主连接' } }, 502)
      const selected = () => accounts.find(account => account.id === body.accountId)
      switch (input.path) {
        case 'api/snapshot': return answer(snapshot())
        case 'api/accounts/selection': {
          const changed = JSON.stringify(hostSelection) !== JSON.stringify(body)
          hostSelection = body
          const effective = nativeAccounts.filter(account => account.enabled && (body.accountIds.includes(account.id) || account.groupIds.some(id => body.groupIds.includes(id))))
          accounts = accounts.filter(account => account.credentialSource !== 'host')
          accounts.push(...effective.map(account => ({ id: account.id, label: account.label, models: ['gpt-6-astra'], credentialSource: 'host' as const, auth: { configured: true, valid: true, status: 'host_managed', message: '使用宿主 OAuth，刷新由宿主管理，BPS 权限尚未验证' } })))
          return published('connected', changed)
        }
        case 'api/accounts/use-host': {
          const account = nativeAccounts.find(account => account.id === body.accountId)
          if (account) accounts.push({ id: account.id, label: account.label, models: ['gpt-6-astra'], credentialSource: 'host', auth: { configured: true, valid: true, status: 'host_managed', message: '使用宿主 OAuth，刷新由宿主管理，BPS 权限尚未验证' } })
          return answer({ status: 'connected' })
        }
        case 'api/accounts/detach': accounts = accounts.filter(account => account.id !== body.accountId); return answer({ status: 'detached' })
        case 'api/onboarding/start':
          polls = 0; onboarding = { operationId: body.operationId, label: body.label || 'BPS 账号', models: body.models || ['gpt-6-astra'], stage: 'pending', prompt: prompt(), expiresAt: Date.now() / 1000 + 900 }
          return answer(onboarding)
        case 'api/onboarding/import':
          try { if (!JSON.parse(body.oauth).access_token) throw new Error() } catch { return answer({ error: { message: 'OAuth 文件缺少 Access Token' } }, 400) }
          accounts.push({ id: `preview-${accounts.length + 1}`, label: body.label || 'BPS 账号', models: body.models, auth: { configured: true, valid: true, status: 'authorized', message: '模拟导入' } })
          return published('authorized')
        case 'api/onboarding/poll':
          if (++polls < 3) return answer({ status: 'pending', retryAfterSeconds: 1 })
          if (onboarding) accounts.push({ id: `preview-${accounts.length + 1}`, label: onboarding.label, models: onboarding.models, auth: { configured: true, valid: true, status: 'authorized', expiresAt: Date.now() / 1000 + 3600, message: '模拟授权' } })
          onboarding = null; return published('authorized')
        case 'api/auth/start': polls = 0; return answer(prompt())
        case 'api/auth/poll':
          if (++polls < 3) return answer({ status: 'pending', retryAfterSeconds: 1 })
          if (selected()) selected()!.auth = { configured: true, valid: true, status: 'authorized', message: '模拟授权' }
          return answer({ status: 'authorized' })
        case 'api/auth/logout':
          if (selected()) selected()!.auth = { configured: false, valid: false, status: 'signed_out', message: '尚未配置 BPS OAuth' }
          return answer({ status: 'signed_out' })
        case 'api/auth/import':
          if (selected()) selected()!.auth = { configured: true, valid: true, status: 'authorized', message: '模拟导入' }
          return answer({ status: 'authorized' })
        case 'api/models': {
          if (params.has('save-error')) return answer({ error: { message: '配置保存冲突，请刷新后重试' } }, 409)
          const changed = JSON.stringify(selected()?.models) !== JSON.stringify(body.models)
          if (selected()) selected()!.models = body.models
          return published('saved', changed)
        }
        default: return answer({ error: { message: '模拟接口不存在' } }, 404)
      }
    },
  }
  window.codexProxyPlugin = bridge
  return true
}
