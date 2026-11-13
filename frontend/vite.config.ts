import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import CodexProxyUI from '@codex-proxy/ui/vite'

export default defineConfig({
  base: './',
  plugins: [vue(), tailwindcss(), CodexProxyUI(), {
    name: 'classic-plugin-script',
    transformIndexHtml: { order: 'post', handler: html => html.replace('type="module" crossorigin', 'defer') },
  }],
  build: {
    modulePreload: false,
    cssCodeSplit: false,
    rolldownOptions: {
      output: { format: 'iife', codeSplitting: false, entryFileNames: 'app.js', assetFileNames: 'app.[ext]' },
    },
  },
})
