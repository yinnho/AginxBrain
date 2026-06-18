import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Desktop client: bundled into the Tauri app, loaded as static assets.
// All API calls go to a remote AginxBrain server (default brain.aginx.net),
// NOT to a local dev server, so no proxy config here.
export default defineConfig({
  plugins: [react()],
  // Tauri serves the built dist/ directly; use relative base so assets
  // resolve regardless of the custom protocol origin.
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
})
