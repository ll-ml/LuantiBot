import { defineConfig } from "vite";

export default defineConfig({
  root: "web",
  server: {
    port: 5173,
    strictPort: false,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:9123",
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/api/, "")
      }
    }
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true
  }
});
