import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'path';

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:38420',
        changeOrigin: true,
        ws: true,
      },
    },
    watch: {
      ignored: ['**/src-tauri/**', '**/crates/**', '**/target/**'],
    },
  },
  build: {
    target: ['es2022', 'chrome110', 'safari16'],
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
        tray: resolve(__dirname, 'tray.html'),
      },
    },
  },
});
