import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 3847,
    strictPort: true,
  },
  build: {
    outDir: "../ui",
    emptyOutDir: true,
  },
});
