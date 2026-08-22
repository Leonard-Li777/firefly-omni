---
name: firefly-omni
description: Universal Multimodal File Intelligence, Text Extraction, Metadata Parsing & Perceptual Deduplication Engine.
---

# firefly-omni Agent Skill

This skill allows AI Agents (Claude Desktop, Cursor, Antigravity, AutoGen) to inspect, classify, extract metadata, convert documents to Markdown, perform OCR, and compute perceptual file similarities.

## Available Actions

### 1. `extract_file_info`
Extracts complete document text, MIME format, EXIF metadata, and OCR text from embedded images.

```json
{
  "path": "/path/to/document.docx",
  "enable_ocr": true,
  "max_content_size_kb": 30
}
```

### 2. `compute_phash`
Computes perceptual hash (pHash) for images to detect visually similar images or video frames.

```json
{
  "path": "/path/to/image.jpg"
}
```

### 3. `detect_file_type`
Uses Google Magika AI model to detect 200+ true MIME file types.

```json
{
  "path": "/path/to/file"
}
```
