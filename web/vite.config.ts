import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      // Gateway default port (config.rs default_port = 8083)
      '/api': 'http://127.0.0.1:8083',
      '/v1': 'http://127.0.0.1:8083',
    },
  },
})
