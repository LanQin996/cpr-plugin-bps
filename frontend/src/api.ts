export interface AuthState { configured: boolean; valid: boolean; status: string; expiresAt?: number | null; message: string }
export interface Account { id: string; label: string; models: string[]; credentialSource?: 'host' | 'independent'; auth: AuthState }
export interface HostAccount { id: string; label: string; email: string | null; enabled: boolean; state: string; groupIds: string[] }
export interface HostGroup { id: string; name: string; enabled: boolean }
export interface HostSelection { groupIds: string[]; accountIds: string[] }
export function accountLabel(account: { id: string; label?: string | null; email?: string | null }): string {
  return account.label?.trim() || account.email?.trim() || account.id
}
export interface Prompt { authorizationUrl: string; userCode: string; interval: number; expiresAt: number }
export interface Onboarding { operationId: string; label: string; models: string[]; stage: string; prompt: Prompt | null; expiresAt: number }
export interface Snapshot { accounts: Account[]; hostAccounts: HostAccount[]; hostGroups: HostGroup[]; hostSelection: HostSelection; defaultModel: string; onboarding: Onboarding | null }
export interface AuthReply { status: string; message?: string; retryAfterSeconds?: number }
export interface Bridge {
  version: number
  request(input: { method: string; path: string; contentType?: string; body?: string }): Promise<{ status: number; contentType: string; body: ArrayBuffer }>
  openExternal?(url: string): Promise<void>
}
declare global { interface Window { codexProxyPlugin?: Bridge } }

export async function request<T>(method: string, path: string, data?: unknown): Promise<T> {
  const bridge = window.codexProxyPlugin
  if (!bridge || bridge.version !== 2) throw new Error('请从宿主插件页面打开 BPS 账号管理')
  const response = await bridge.request({ method, path, ...(data === undefined ? {} : { contentType: 'application/json', body: JSON.stringify(data) }) })
  if (response.contentType.split(';')[0].trim() !== 'application/json') throw new Error('插件返回了不支持的内容类型')
  let value: unknown
  try { value = JSON.parse(new TextDecoder().decode(response.body)) }
  catch { throw new Error('插件返回了无效 JSON') }
  if (response.status < 200 || response.status >= 300) {
    const error = value as { error?: { message?: unknown } }
    throw new Error(typeof error?.error?.message === 'string' ? error.error.message : `请求失败（HTTP ${response.status}）`)
  }
  return value as T
}

export function operationId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 15) | 64
  bytes[8] = (bytes[8] & 63) | 128
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}
