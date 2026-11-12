import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const backend = process.env.TASKCRAFT_BACKEND ?? 'http://127.0.0.1:8788'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5200,
    proxy: {
      '/api': { target: backend, changeOrigin: false },
    },
  },
  build: {
    chunkSizeWarningLimit: 1500,
  },
})
