---
name: firefly-omni
description: Universal Multimodal File Intelligence, Text Extraction, Metadata Parsing & Perceptual Deduplication Engine.
---

# firefly-omni Agent Skill & Integration Contract

`firefly-omni` is a high-performance Rust multimodal file extraction & intelligence engine. It provides AI agents (Antigravity, Cursor, Claude Desktop, AutoGen) with instant MIME classification, document text parsing, embedded image OCR substitution, perceptual hash deduplication, and corruption checks.

---

## 🚀 Key Interfaces

### 1. Model Context Protocol (MCP Server `omni-mcp`)
Run the Stdin/Stdout JSON-RPC MCP server:
```bash
cargo run -p omni-mcp
```

#### Available MCP Tools
- **`omni_detect_mime`**: Detect precise MIME file type using Google Magika ONNX neural network.
  - Arguments: `{ "path": "/path/to/file" }`
- **`omni_compute_phash`**: Calculate 64-bit perceptual hash (pHash) for image deduplication.
  - Arguments: `{ "path": "/path/to/image.png" }`
- **`omni_extract_ocr`**: Parse Markdown text and substitute inline embedded image OCR placeholders.
  - Arguments: `{ "markdown": "![image](data:image/png;base64,...)" }`
- **`omni_check_corrupted`**: Verify whether an archive or media file is damaged or corrupted.
  - Arguments: `{ "path": "/path/to/archive.zip" }`

---

### 2. Axum REST API Server (`omni-server`)
Start the web server (default port `8080`):
```bash
cargo run -p omni-server
```

#### Endpoints
- `GET /health` -> Health check & engine status.
- `GET /api/config` -> Retrieve current worker & ORT configuration.
- `POST /api/extract` (Multipart Form) -> Upload file for full MIME detection, pHash computation, and text extraction.

---

### 3. Node.js Native C++ / Rust Addon (`@firefly/omni` / `omni-node`)
Used by `apps/desktop` for zero-overhead native bindings:
```typescript
import { computePhashNative, checkCorruptedNative, replaceEmbeddedImageOcrNative } from '@firefly/omni'

const phash = computePhashNative('/path/to/image.jpg')
const isCorrupted = checkCorruptedNative('/path/to/archive.zip')
```

---

### 4. React Web Drag & Drop Console (`apps/omni/ui`)
Modern Web UI for batch drag-and-drop inspection:
```bash
cd apps/omni/ui
pnpm dev
```
Features real-time file inspection, Magika MIME detection, pHash comparison, and Markdown preview.
