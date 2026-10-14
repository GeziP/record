import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

// WebView2 是常青版 Chromium，不需要为旧浏览器降级编译
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: 'chrome120', outDir: 'dist', emptyOutDir: true },
  test: { environment: 'node', include: ['src/**/*.test.ts'] },
});
