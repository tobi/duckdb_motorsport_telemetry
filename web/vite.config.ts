import { defineConfig } from 'vite';

export default defineConfig({
  base: '/duckdb_motorsport_telemetry/',
  build: { target: 'es2022', sourcemap: true },
  worker: { format: 'es' },
});
