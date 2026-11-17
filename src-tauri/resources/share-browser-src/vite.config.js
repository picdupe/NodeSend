import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

const runtimeLanguageScript = {
  name: 'nodesend-runtime-language-script',
  enforce: 'pre',
  transformIndexHtml: {
    order: 'pre',
    handler(html) {
      return html.replace(
        /<script\b(?=[^>]*\btype=["']module["'])(?=[^>]*\bsrc=["']\/share-ui\/language\.js["'])[^>]*>\s*<\/script>|<link\b(?=[^>]*\bhref=["']\/share-ui\/webui\.css["'])[^>]*>/gi,
        '',
      )
    },
  },
}

export default defineConfig({
  base: '/share-ui/',
  plugins: [runtimeLanguageScript, vue()],
  build: {
    outDir: '../share-browser',
    emptyOutDir: false,
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
        entryFileNames: 'webui.js',
        assetFileNames: asset => asset.name?.endsWith('.css') ? 'webui.css' : '[name][extname]',
      },
    },
  },
})
