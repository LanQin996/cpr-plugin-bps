import { createApp } from 'vue'
import App from './App.vue'
import '@codex-proxy/ui/styles.css'
import './style.css'
import { installPreview } from './preview'

const preview = installPreview()
createApp(App, { preview }).mount('#app')
