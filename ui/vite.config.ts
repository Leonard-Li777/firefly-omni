import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 8199,
    proxy: {
      '/health': {
        target: 'http://localhost:8080',
        changeOrigin: true,
        configure: (proxy) => {
          proxy.on('error', (_err, _req, res) => {
            // 当后端 omni-server 未启动时，优雅返回离线 Mock 数据而不是 500
            res.writeHead(200, { 'Content-Type': 'application/json' })
            res.end(JSON.stringify({ status: 'offline', server: 'firefly-omni' }))
          })
        }
      },
      '/api': {
        target: 'http://localhost:8080',
        changeOrigin: true,
        configure: (proxy) => {
          proxy.on('error', (_err, _req, res) => {
            res.writeHead(503, { 'Content-Type': 'application/json' })
            res.end(JSON.stringify({ error: 'omni-server is offline' }))
          })
        }
      }
    }
  }
})
