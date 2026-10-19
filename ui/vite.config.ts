import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'node:path';

// Two extension pages (popup, options). The output keeps the extension layout (popup/popup.html, options/options.html) so
// tools/build-extension.mjs can overlay ui/dist onto the extension folder. Everything is bundled: MV3 pages allow no inline
// scripts and no remote code.
export default defineConfig({
  base: './',
  plugins: [react(), tailwindcss()],
  resolve: { alias: { '@': path.resolve(import.meta.dirname, 'src') } },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    assetsDir: 'ui-assets',
    target: 'chrome120',
    modulePreload: false,
    cssCodeSplit: false,
    rollupOptions: { input: { popup: path.resolve(import.meta.dirname, 'popup/popup.html'), options: path.resolve(import.meta.dirname, 'options/options.html') } },
  },
});
