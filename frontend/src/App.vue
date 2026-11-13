<script setup lang="ts">
import { ZButton, ZCheckbox, ZConfigProvider, ZDialog, ZFormItem, ZInput, ZTag, ZTextarea } from '@codex-proxy/ui'
import { zhHans } from '@codex-proxy/ui/locale'
import { ArrowUpRight, CircleCheck, KeyRound, Plus, RefreshCw, Unplug } from '@lucide/vue'
import { useAccounts } from './useAccounts'
import { accountLabel } from './api'

defineProps<{ preview: boolean }>()
const { snapshot, loading, busy, reloadRequired, error, notice, dialog, modelsDialog, hostDialog, selectedGroups, selectedAccounts, effectiveAccounts, byGroup, toggleGroup, toggleAccount, clearSelection, saveSelection, selected, adding, mode, label, modelsText, prompt, stage, expired, fileName, fileContent, dialogError, refresh, open, resume, poll, login, readFile, importFile, openAuthorization, copyCode, logout, editModels, saveModels } = useAccounts()
function expiry(value?: number | null) { return value ? `${new Date(value * 1000).toISOString().slice(0, 16).replace('T', ' ')} UTC` : '未提供有效期' }
</script>

<template>
  <ZConfigProvider :locale="zhHans">
    <main class="flex min-w-0 flex-col gap-4 font-sans text-z-text" :class="{ 'p-5': preview }" :aria-busy="loading">
      <div v-if="preview" class="rounded-z bg-z-warning-container px-3 py-2 text-z-sm text-z-warning-on-container" role="note">模拟预览，不会登录真实账号或修改宿主</div>
      <div v-if="reloadRequired" role="status" class="rounded-z bg-z-success-container px-4 py-3 text-z-sm text-z-success-on-container">设置已保存并生效。继续管理前，请点击宿主页面右上角的刷新按钮。</div>
      <fieldset :disabled="reloadRequired" class="m-0 flex min-w-0 flex-col gap-4 border-0 p-0" aria-label="账号管理操作">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <div class="flex items-center gap-2"><span class="font-emphasis">已接入账号</span><ZTag>{{ snapshot.accounts.length }} / 256</ZTag></div>
        <div class="flex items-center gap-2">
          <ZButton variant="text" :loading="loading" :disabled="busy" aria-label="刷新账号状态" @click="refresh"><template #icon><RefreshCw class="size-4" /></template>刷新</ZButton>
          <ZButton type="primary" :disabled="busy || loading" @click="hostDialog = true"><template #icon><Plus class="size-4" /></template>选择分组和账号</ZButton>
          <ZButton v-if="snapshot.accounts.length" variant="text" :disabled="busy || loading || snapshot.accounts.length >= 256" @click="open()">独立 BPS 授权</ZButton>
        </div>
      </div>
      <div v-if="error" role="alert" class="rounded-z bg-z-error-container px-4 py-3 text-z-sm text-z-error-on-container">{{ error }}</div>
      <div v-if="notice" role="status" class="flex items-center gap-2 text-z-sm text-z-success-text"><CircleCheck class="size-4 shrink-0" />{{ notice }}</div>
      <section v-if="snapshot.onboarding" class="flex flex-wrap items-center justify-between gap-3 rounded-z-card bg-z-warning-container px-4 py-3 text-z-warning-on-container">
        <div><div class="font-emphasis">{{ snapshot.onboarding.label }} · 接入尚未完成</div><div class="mt-1 text-z-sm">可以继续浏览器授权或恢复账号接入，不需要重新填写配置</div></div>
        <ZButton :disabled="busy" @click="resume">继续接入</ZButton>
      </section>
      <section v-if="!loading && !snapshot.accounts.length" class="flex flex-col items-start gap-4 rounded-z-card bg-z-bg-container p-6 shadow-z-card sm:flex-row sm:items-center sm:gap-5">
        <div class="flex size-12 shrink-0 items-center justify-center rounded-z bg-z-primary-container text-z-primary-on-container"><KeyRound class="size-6" /></div>
        <div class="flex-1"><h2 class="m-0 text-z-lg font-emphasis">直接使用宿主已有账号</h2><p class="mb-0 mt-2 text-z-sm text-z-text-secondary">选择已经登录的 OpenAI OAuth 账号即可接入，沿用宿主凭据刷新和代理，无需重新登录</p><ZButton class="mt-3" variant="link" :disabled="busy" @click="snapshot.onboarding ? resume() : open()">{{ snapshot.onboarding ? '继续独立授权' : '也可以使用独立 BPS 授权或导入文件' }}</ZButton></div>
      </section>
      <section v-for="account in snapshot.accounts" :key="account.id" class="rounded-z-card bg-z-bg-container p-5 shadow-z-card">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div class="flex min-w-0 flex-wrap items-center gap-3"><h2 class="m-0 wrap-anywhere text-z-base font-emphasis">{{ account.label }}</h2><ZTag :type="account.auth.valid ? 'success' : account.auth.configured ? 'warning' : 'default'">{{ account.credentialSource === 'host' ? account.auth.valid ? '复用宿主登录' : '宿主账号不可用' : account.auth.valid ? '已授权' : account.auth.configured ? '需要重新授权' : '未登录' }}</ZTag></div>
          <div class="flex flex-wrap items-center gap-1">
            <ZButton v-if="account.credentialSource !== 'host'" size="small" :type="account.auth.valid ? 'default' : 'primary'" :disabled="busy" @click="open(account)">{{ account.auth.valid ? '重新授权' : '登录' }}</ZButton>
            <ZButton v-if="account.credentialSource !== 'host'" size="small" variant="text" :disabled="busy" @click="open(account, 'import')">导入 OAuth</ZButton>
            <ZButton size="small" variant="text" :disabled="busy" @click="editModels(account)">模型设置</ZButton>
            <ZButton v-if="account.credentialSource === 'host'" size="small" variant="text" :disabled="busy" @click="hostDialog = true">调整接入范围</ZButton>
            <ZButton v-else-if="account.auth.configured" size="small" variant="text" :disabled="busy" @click="logout(account)"><template #icon><Unplug class="size-4" /></template>退出登录</ZButton>
          </div>
        </div>
        <div class="mt-3 flex flex-wrap items-center gap-2"><ZTag v-for="model in account.models" :key="model" size="small">{{ model }}</ZTag></div>
        <p class="mb-0 mt-3 text-z-sm text-z-text-secondary">{{ account.credentialSource === 'host' ? account.auth.message : account.auth.valid ? `有效期：${expiry(account.auth.expiresAt)}` : account.auth.message }}</p>
      </section>
      <p class="m-0 text-z-sm text-z-text-tertiary">接入后所选账号的指定模型改走 BPS，取消接入恢复原通道，BPS 访问权限以实际返回为准</p>

      <ZDialog v-model="hostDialog" title="选择分组和账号" description="分组与单账号可以同时多选，按并集启用，重复账号只计算一次" :width="720" :close-on-click-modal="!busy">
        <div class="flex flex-col gap-3">
          <div v-if="error" role="alert" class="rounded-z bg-z-error-container px-3 py-2 text-z-sm text-z-error-on-container">{{ error }}</div>
          <p class="m-0 text-z-sm text-z-text-secondary">启用后默认模型 {{ snapshot.defaultModel }} 将通过 BPS 执行，可随时在模型设置中调整</p>
          <section><h3 class="mb-2 mt-1 text-z-sm font-emphasis">分组多选</h3><div class="grid gap-2 sm:grid-cols-2"><div v-for="group in snapshot.hostGroups" :key="group.id" class="rounded-z bg-z-fill-quaternary px-3 py-2"><ZCheckbox :model-value="selectedGroups.includes(group.id)" :label="`${group.name}${group.enabled ? '' : '（已停用）'}`" :disabled="busy || (!group.enabled && !selectedGroups.includes(group.id))" @update:model-value="toggleGroup(group.id, $event)" /></div></div><p v-if="!snapshot.hostGroups.length" class="text-z-sm text-z-text-secondary">宿主暂无分组，仍可选择下方单账号</p></section>
          <section><h3 class="mb-1 mt-1 text-z-sm font-emphasis">单账号多选</h3><p class="mb-3 mt-0 text-z-sm text-z-text-secondary">包含无分组账号，可独立勾选，也可与分组同时选择</p><div class="flex max-h-60 flex-col gap-2 overflow-y-auto"><div v-for="account in snapshot.hostAccounts" :key="account.id" class="flex min-w-0 flex-wrap items-center justify-between gap-2 rounded-z bg-z-fill-quaternary px-3 py-2"><ZCheckbox class="min-w-0 flex-1 py-1" :model-value="selectedAccounts.includes(account.id)" :label="accountLabel(account)" :disabled="busy || (!account.enabled && !selectedAccounts.includes(account.id))" @update:model-value="toggleAccount(account.id, $event)" /><div class="flex gap-2"><ZTag v-if="!account.groupIds.length" size="small">无分组</ZTag><ZTag v-if="byGroup(account) && account.enabled" type="info" size="small">分组已覆盖</ZTag><span v-if="!account.enabled" class="text-z-sm text-z-warning-text">宿主已停用</span></div></div></div><p v-if="!snapshot.hostAccounts.length" class="text-z-sm text-z-text-secondary">没有 OpenAI OAuth 账号，可先在宿主添加账号</p></section>
          <section class="rounded-z bg-z-primary-container px-3 py-3 text-z-primary-on-container"><div class="flex flex-wrap items-center justify-between gap-2"><strong class="text-z-sm">最终生效 {{ effectiveAccounts.length }} 个账号</strong><ZButton size="small" variant="text" :disabled="busy" @click="clearSelection">清空选择</ZButton></div><div class="mt-2 flex max-h-28 flex-wrap gap-1.5 overflow-y-auto"><ZTag v-for="account in effectiveAccounts" :key="account.id" size="small">{{ accountLabel(account) }}</ZTag><span v-if="!effectiveAccounts.length" class="text-z-sm">保存后取消所有宿主账号的 BPS 接入，独立 BPS 账号保持不变</span></div></section>
          <p class="m-0 text-z-sm text-z-text-tertiary">按保存时的分组成员生效，分组成员变化后请刷新并重新应用选择，不改变宿主账号启用状态或 Key 权限</p>
        </div>
        <template #footer><ZButton :disabled="busy" @click="hostDialog = false">取消</ZButton><ZButton type="primary" :loading="busy" @click="saveSelection">应用选择（{{ effectiveAccounts.length }}）</ZButton></template>
      </ZDialog>

      <ZDialog v-model="dialog" :title="adding ? '添加 BPS 账号' : `授权 ${selected?.label ?? 'BPS 账号'}`" :width="520" :close-on-click-modal="!busy">
        <div class="flex flex-col gap-4">
          <div v-if="dialogError" role="alert" class="rounded-z bg-z-error-container px-3 py-2 text-z-sm text-z-error-on-container">{{ dialogError }}</div>
          <template v-if="!prompt && !stage">
            <div class="flex gap-2"><ZButton size="small" :type="mode === 'login' ? 'primary' : 'default'" :aria-pressed="mode === 'login'" :disabled="busy" @click="mode = 'login'">浏览器授权</ZButton><ZButton size="small" :type="mode === 'import' ? 'primary' : 'default'" :aria-pressed="mode === 'import'" :disabled="busy" @click="mode = 'import'">导入文件</ZButton></div>
            <ZFormItem v-if="adding" label="账号名称（可选）"><ZInput v-model="label" placeholder="例如：个人账号" :disabled="busy" aria-label="账号名称" /></ZFormItem>
            <p v-if="mode === 'login'" class="m-0 text-z-sm text-z-text-secondary">获取设备码后，在浏览器完成授权，插件会自动检查结果并接入账号</p>
            <div v-else class="flex flex-col gap-2"><label class="text-z-sm font-emphasis" for="oauth-file">选择独立 BPS OAuth 文件</label><input id="oauth-file" type="file" accept=".json,application/json" :disabled="busy" @change="readFile"><span class="text-z-sm text-z-text-secondary">{{ fileName || '仅接收不超过 48 KiB 的 JSON 文件，不读取 Codex 登录文件' }}</span></div>
            <details v-if="adding" class="text-z-sm"><summary class="cursor-pointer text-z-text-secondary">模型选项</summary><div class="mt-3"><ZFormItem label="模型 ID"><ZTextarea v-model="modelsText" :rows="3" :disabled="busy" aria-label="模型 ID" /></ZFormItem><p class="mb-0 text-z-text-secondary">预填原项目示例模型，实际权限以生成结果为准，每行一个模型 ID</p></div></details>
          </template>
          <template v-else-if="prompt">
            <p class="m-0 text-z-sm text-z-text-secondary">在授权页面输入下面的设备码，完成后将自动接入</p>
            <ZInput id="device-code" :model-value="prompt.userCode" readonly aria-label="设备码" class="font-mono text-z-xl" />
            <div class="flex flex-wrap gap-2"><ZButton type="primary" :disabled="expired" @click="openAuthorization"><template #icon><ArrowUpRight class="size-4" /></template>打开授权页面</ZButton><ZButton @click="copyCode">复制设备码</ZButton></div>
            <details class="text-z-sm text-z-text-secondary"><summary class="cursor-pointer">手动打开授权地址</summary><p class="wrap-anywhere select-all">{{ prompt.authorizationUrl }}</p></details>
            <div class="rounded-z bg-z-fill-quaternary px-3 py-2 text-z-sm text-z-text-secondary" role="status">{{ expired ? '设备码已过期，请关闭后重新发起授权' : '等待浏览器授权，正在自动检查' }}</div>
          </template>
          <p v-else class="m-0 text-z-sm text-z-text-secondary">授权已保存，正在完成宿主账号接入，可以点击下方按钮继续</p>
        </div>
        <template #footer>
          <ZButton :disabled="busy" @click="dialog = false">{{ prompt || stage ? '稍后继续' : '取消' }}</ZButton>
          <ZButton v-if="!prompt && !stage" type="primary" :loading="busy" :disabled="mode === 'import' && !fileContent" @click="mode === 'login' ? login() : importFile()">{{ mode === 'login' ? '获取设备码' : '导入并接入' }}</ZButton>
          <ZButton v-else type="primary" :loading="busy" :disabled="expired" @click="poll">{{ prompt ? '我已完成授权' : '继续完成接入' }}</ZButton>
        </template>
      </ZDialog>
      <ZDialog v-model="modelsDialog" title="模型设置" description="仅修改当前账号的模型，不改变其他账号或 Key 权限" :width="480" :close-on-click-modal="!busy">
        <div v-if="dialogError" role="alert" class="mb-3 text-z-error-text">{{ dialogError }}</div>
        <ZFormItem label="模型 ID，每行一个"><ZTextarea v-model="modelsText" :rows="5" :disabled="busy" aria-label="模型 ID，每行一个" /></ZFormItem>
        <template #footer><ZButton :disabled="busy" @click="modelsDialog = false">取消</ZButton><ZButton type="primary" :loading="busy" @click="saveModels">保存</ZButton></template>
      </ZDialog>
      </fieldset>
    </main>
  </ZConfigProvider>
</template>
