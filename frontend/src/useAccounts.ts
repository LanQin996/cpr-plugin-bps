import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { operationId, request } from './api'
import type { Account, AuthReply, HostAccount, MutationReply, Onboarding, Prompt, Snapshot } from './api'

export function useAccounts() {
  const snapshot = ref<Snapshot>({ accounts: [], hostAccounts: [], hostGroups: [], hostSelection: { groupIds: [], accountIds: [] }, defaultModel: 'gpt-6-astra', onboarding: null })
  const hostDialog = ref(false)
  const selectedGroups = ref<string[]>([]), selectedAccounts = ref<string[]>([])
  const byGroup = (account: HostAccount) => account.groupIds.some(id => selectedGroups.value.includes(id))
  const effectiveAccounts = computed(() => snapshot.value.hostAccounts.filter(account => account.enabled && (selectedAccounts.value.includes(account.id) || byGroup(account))))
  function toggleGroup(id: string, checked: boolean) { selectedGroups.value = checked ? [...new Set([...selectedGroups.value, id])] : selectedGroups.value.filter(value => value !== id) }
  function toggleAccount(id: string, checked: boolean) { selectedAccounts.value = checked ? [...new Set([...selectedAccounts.value, id])] : selectedAccounts.value.filter(value => value !== id) }
  function clearSelection() { selectedGroups.value = []; selectedAccounts.value = [] }
  const loading = ref(true), busy = ref(false), error = ref(''), notice = ref(''), reloadRequired = ref(false)
  const dialog = ref(false), modelsDialog = ref(false), selected = ref<Account | null>(null)
  const mode = ref<'login' | 'import'>('login'), label = ref(''), modelsText = ref('gpt-6-astra')
  const prompt = ref<Prompt | null>(null), setup = ref<Onboarding | null>(null), stage = ref('')
  const fileName = ref(''), fileContent = ref(''), dialogError = ref('')
  const clock = ref(Date.now())
  let operation = '', timer: ReturnType<typeof setTimeout> | undefined, epoch = 0, alive = true
  const adding = computed(() => !selected.value)
  const expired = computed(() => !!prompt.value && clock.value >= prompt.value.expiresAt * 1000)
  const models = () => Array.from(new Set(modelsText.value.split(/[\s,，]+/).map(value => value.trim()).filter(Boolean)))
  function stop() { if (timer) clearTimeout(timer); timer = undefined }
  async function refresh() {
    if (reloadRequired.value || busy.value) return
    loading.value = true
    try {
      const value = await request<Snapshot>('GET', 'api/snapshot')
      if (!Array.isArray(value.accounts) || !Array.isArray(value.hostAccounts) || !Array.isArray(value.hostGroups) || !value.hostSelection || typeof value.defaultModel !== 'string') throw new Error('账号状态格式无效')
      if (alive) { snapshot.value = value; error.value = '' }
    } catch (failure) { if (alive) error.value = message(failure) }
    finally { if (alive) loading.value = false }
  }
  async function saved(result: MutationReply) {
    if (result.snapshot) snapshot.value = result.snapshot
    reloadRequired.value = result.reloadRequired === true
    // 更新实例配置会使当前宿主桥失效，直接使用保存响应中的快照。
    if (!result.snapshot && !reloadRequired.value) {
      const value = await request<Snapshot>('GET', 'api/snapshot')
      if (alive) snapshot.value = value
    }
  }
  function open(account: Account | null = null, method: 'login' | 'import' = 'login') {
    epoch++; stop(); selected.value = account; mode.value = method; prompt.value = null; setup.value = null
    operation = operationId(); label.value = account?.label ?? ''; modelsText.value = (account?.models ?? [snapshot.value.defaultModel]).join('\n')
    stage.value = ''; fileContent.value = ''; fileName.value = ''; dialogError.value = ''; dialog.value = true
  }
  function queue(seconds: number) { stop(); if (dialog.value) timer = setTimeout(() => void poll(), Math.max(1, seconds) * 1000) }
  async function poll() {
    if (busy.value || !dialog.value) return
    clock.value = Date.now()
    if (expired.value) { stop(); dialogError.value = '设备码已过期，请关闭后重新发起授权'; return }
    const current = epoch; busy.value = true; dialogError.value = ''
    try {
      const result = await request<AuthReply>('POST', adding.value ? 'api/onboarding/poll' : 'api/auth/poll', adding.value ? { operationId: operation } : { accountId: selected.value?.id })
      if (!alive || current !== epoch) return
      if (result.status === 'authorized') {
        notice.value = adding.value ? '账号已授权并自动接入' : '账号授权已更新'
        dialog.value = false
        await saved(result)
      } else { queue(result.retryAfterSeconds ?? prompt.value?.interval ?? 5) }
    } catch (failure) { if (alive && current === epoch) dialogError.value = message(failure) }
    finally { if (alive) busy.value = false }
  }
  async function login() {
    const current = epoch; busy.value = true; dialogError.value = ''
    try {
      if (adding.value) {
        const result = await request<Onboarding & AuthReply>('POST', 'api/onboarding/start', { operationId: operation, label: label.value, models: models() })
        if (!alive || current !== epoch) return
        if (result.status === 'authorized') { dialog.value = false; notice.value = '账号已接入'; await saved(result); return }
        setup.value = result; prompt.value = result.prompt; stage.value = result.stage
      } else {
        const result = await request<Prompt>('POST', 'api/auth/start', { accountId: selected.value?.id })
        if (!alive || current !== epoch) return
        prompt.value = result; stage.value = 'pending'
      }
      if (prompt.value) queue(prompt.value.interval)
    } catch (failure) { if (alive && current === epoch) dialogError.value = message(failure) }
    finally { if (alive) busy.value = false }
  }
  function resume() {
    const value = snapshot.value.onboarding
    if (!value) return
    open(); operation = value.operationId; setup.value = value; label.value = value.label; modelsText.value = value.models.join('\n'); prompt.value = value.prompt; stage.value = value.stage
    if (value.stage !== 'pending') void poll()
    else if (value.prompt && value.expiresAt * 1000 > Date.now()) queue(value.prompt.interval)
    else if (!value.prompt) void login()
  }
  async function readFile(event: Event) {
    const input = event.target as HTMLInputElement, file = input.files?.[0], current = epoch
    fileContent.value = ''; fileName.value = ''; dialogError.value = ''
    if (!file) return
    try {
      if (file.size > 48 * 1024) throw new Error('OAuth 文件不能超过 48 KiB')
      const text = await file.text(); JSON.parse(text)
      if (alive && current === epoch) { fileName.value = file.name; fileContent.value = text }
    } catch (failure) { if (alive && current === epoch) dialogError.value = message(failure) }
    finally { input.value = '' }
  }
  async function importFile() {
    const current = epoch; busy.value = true; dialogError.value = ''
    try {
      const result = await request<AuthReply>('POST', adding.value ? 'api/onboarding/import' : 'api/auth/import', adding.value ? { operationId: operation, label: label.value, models: models(), oauth: fileContent.value } : { accountId: selected.value?.id, oauth: fileContent.value })
      if (!alive || current !== epoch) return
      if (result.status !== 'authorized') throw new Error('账号接入尚未完成，请检查授权状态')
      dialog.value = false; notice.value = 'OAuth 已导入并接入'; await saved(result)
    } catch (failure) { if (alive && current === epoch) dialogError.value = message(failure) }
    finally { fileContent.value = ''; if (alive) busy.value = false }
  }
  async function openAuthorization() {
    try {
      if (!prompt.value || !window.codexProxyPlugin?.openExternal) throw new Error('当前宿主不支持打开外部页面，请复制下方授权地址')
      await window.codexProxyPlugin.openExternal(prompt.value.authorizationUrl)
    } catch (failure) { dialogError.value = message(failure) }
  }
  async function copyCode() {
    try { await navigator.clipboard.writeText(prompt.value?.userCode ?? ''); notice.value = '设备码已复制' }
    catch { (document.getElementById('device-code') as HTMLInputElement | null)?.select(); dialogError.value = '请手动选择并复制设备码' }
  }
  async function logout(account: Account) {
    busy.value = true; error.value = ''
    try { const result = await request<AuthReply>('POST', 'api/auth/logout', { accountId: account.id }); if (alive) { notice.value = '已退出登录'; await saved(result) } }
    catch (failure) { if (alive) error.value = message(failure) }
    finally { if (alive) busy.value = false }
  }
  async function saveSelection() {
    busy.value = true; error.value = ''
    try {
      const result = await request<MutationReply>('POST', 'api/accounts/selection', { groupIds: selectedGroups.value, accountIds: selectedAccounts.value })
      if (alive) { hostDialog.value = false; await saved(result); notice.value = `已按分组与单账号的并集接入 ${snapshot.value.accounts.filter(account => account.credentialSource === 'host').length} 个账号` }
    } catch (failure) { if (alive) error.value = message(failure) }
    finally { if (alive) busy.value = false }
  }
  function editModels(account: Account) { selected.value = account; modelsText.value = account.models.join('\n'); dialogError.value = ''; modelsDialog.value = true }
  async function saveModels() {
    busy.value = true; dialogError.value = ''
    try { const result = await request<MutationReply>('POST', 'api/models', { accountId: selected.value?.id, models: models() }); if (alive) { modelsDialog.value = false; await saved(result); notice.value = '模型已保存' } }
    catch (failure) { if (alive) dialogError.value = message(failure) }
    finally { if (alive) busy.value = false }
  }
  watch(dialog, open => { if (!open) { epoch++; stop(); fileContent.value = ''; fileName.value = '' } })
  watch(hostDialog, open => { if (open) { selectedGroups.value = [...snapshot.value.hostSelection.groupIds]; selectedAccounts.value = [...snapshot.value.hostSelection.accountIds]; error.value = '' } })
  watch(mode, () => { fileContent.value = ''; fileName.value = ''; dialogError.value = '' })
  onMounted(refresh)
  onBeforeUnmount(() => { alive = false; epoch++; stop(); fileContent.value = '' })
  return { snapshot, loading, busy, reloadRequired, error, notice, dialog, modelsDialog, hostDialog, selectedGroups, selectedAccounts, effectiveAccounts, byGroup, toggleGroup, toggleAccount, clearSelection, saveSelection, selected, adding, mode, label, modelsText, prompt, setup, stage, expired, fileName, fileContent, dialogError, refresh, open, resume, poll, login, readFile, importFile, openAuthorization, copyCode, logout, editModels, saveModels }
}
function message(value: unknown) { return value instanceof Error ? value.message : '无法完成当前账号操作' }
