# Plan 023: Markdown Rendering in File Viewer

## Overview

Add the ability to view markdown files with rich formatting (rendered HTML) instead of just plain text. Similar to the existing "Wrap" toggle, add a "Format" toggle to switch between raw markdown and rendered preview.

## Motivation

Currently markdown files (`.md`, `.markdown`) are displayed as plain text with syntax highlighting. This makes it hard to read documentation, READMEs, and other markdown content. Rendering markdown provides:

- **Better readability** - Headers, lists, links, tables displayed properly
- **Preview mode** - See how the markdown looks when rendered
- **Hybrid workflow** - Toggle between raw (for editing/copying) and formatted (for reading)

## Design Decisions

### Client-Side Rendering

**Decision**: Render markdown in the browser using a JavaScript library.

**Rationale**:
- No server changes needed
- Instant switching between raw/rendered
- Reduces server load
- Simpler implementation

**Library choice**: `marked` (lightweight, fast, CommonMark + GFM support)

**Alternatives considered**:
- `markdown-it` - More extensible but heavier
- `remark` - More powerful but complex
- Server-side rendering - Adds complexity, requires API changes

### UI Design

**Toggle button** in FileViewer toolbar next to "Wrap":
```
[⊞ Wrap]  [📝 Format]
```

**States**:
- Format OFF (default): Show syntax-highlighted raw markdown
- Format ON: Show rendered HTML with proper styling

**Preference storage**: Save in `$profile.markdownFormat` (boolean)

**Auto-detect**: Only show Format button for `.md` and `.markdown` files

### Rendering Features

**Support**:
- ✅ Headers (h1-h6)
- ✅ Lists (ordered, unordered, nested)
- ✅ Links (clickable)
- ✅ Code blocks with syntax highlighting
- ✅ Inline code
- ✅ Blockquotes
- ✅ Tables (GFM)
- ✅ Bold, italic, strikethrough
- ✅ Horizontal rules
- ✅ Images (if accessible)

**Security**: Sanitize HTML to prevent XSS (marked has built-in sanitization)

### Styling

**Theme-aware CSS** matching the app's dark theme:
- Headers: Larger, bold, with bottom borders
- Links: Accent color, hover effects
- Code blocks: Same syntax highlighting as file viewer
- Tables: Bordered, zebra-striping
- Blockquotes: Left border, muted background

## Implementation Plan

### 1. Add Dependencies

**Package**: `marked` (markdown parser)

```bash
cd web
pnpm add marked
```

### 2. Update FileViewer Component

**File**: `web/src/lib/FileViewer.svelte`

**Changes**:
- Add `markdownFormat` state from profile
- Detect if file is markdown (by extension)
- Add `toggleMarkdownFormat()` function
- Add "Format" button to toolbar (conditionally shown)
- Render content as HTML when format=true
- Add markdown-specific CSS styles

**Implementation**:
```typescript
import { marked } from 'marked';

// Detect markdown files
$: isMarkdown = path.endsWith('.md') || path.endsWith('.markdown');

// Preference from profile
$: markdownFormat = $profile.markdownFormat ?? false;

function toggleMarkdownFormat() {
    $profile.markdownFormat = !markdownFormat;
}

// Render markdown to HTML
$: renderedMarkdown = markdownFormat && isMarkdown
    ? marked.parse(rawContent, { gfm: true, breaks: true })
    : '';
```

**Template changes**:
```svelte
<!-- Toolbar -->
{#if isMarkdown}
    <button class="toolbar-btn" on:click={toggleMarkdownFormat}>
        📝 {markdownFormat ? 'Raw' : 'Format'}
    </button>
{/if}

<!-- Content display -->
{#if markdownFormat && isMarkdown}
    <div class="markdown-content">
        {@html renderedMarkdown}
    </div>
{:else}
    <!-- Existing code table -->
{/if}
```

### 3. Add Markdown Styles

**File**: `web/src/lib/FileViewer.svelte` (in `<style>`)

**Add**:
```css
.markdown-content {
    padding: 24px;
    max-width: 900px;
    margin: 0 auto;
    color: var(--text);
    line-height: 1.6;
}

.markdown-content h1,
.markdown-content h2,
.markdown-content h3 {
    border-bottom: 1px solid var(--border);
    padding-bottom: 0.3em;
    margin-top: 24px;
    margin-bottom: 16px;
}

.markdown-content h1 { font-size: 2em; }
.markdown-content h2 { font-size: 1.5em; }
.markdown-content h3 { font-size: 1.25em; }

.markdown-content a {
    color: var(--accent);
    text-decoration: none;
}

.markdown-content a:hover {
    text-decoration: underline;
}

.markdown-content code {
    background: var(--bg-secondary);
    padding: 0.2em 0.4em;
    border-radius: 3px;
    font-family: var(--font-mono);
    font-size: 0.9em;
}

.markdown-content pre {
    background: var(--bg-secondary);
    padding: 16px;
    border-radius: 6px;
    overflow-x: auto;
}

.markdown-content pre code {
    background: none;
    padding: 0;
}

.markdown-content blockquote {
    border-left: 4px solid var(--accent);
    padding-left: 16px;
    margin-left: 0;
    color: var(--text-muted);
}

.markdown-content table {
    border-collapse: collapse;
    width: 100%;
    margin: 16px 0;
}

.markdown-content th,
.markdown-content td {
    border: 1px solid var(--border);
    padding: 8px 12px;
    text-align: left;
}

.markdown-content th {
    background: var(--bg-secondary);
    font-weight: 600;
}

.markdown-content tr:nth-child(even) {
    background: var(--bg-hover);
}

.markdown-content img {
    max-width: 100%;
    height: auto;
}

.markdown-content ul,
.markdown-content ol {
    padding-left: 2em;
}

.markdown-content li {
    margin: 0.25em 0;
}
```

### 4. Update Profile Type

**File**: `web/src/lib/profile.ts`

**Add**:
```typescript
export interface Profile {
    // ... existing fields
    markdownFormat?: boolean;
}
```

### 5. Add Syntax Highlighting to Code Blocks

**Optional enhancement**: Use the existing `highlightFile()` function for code blocks within markdown.

**Implementation**:
```typescript
import { highlightFile } from '$lib/highlight';

// Configure marked to use syntax highlighting
marked.setOptions({
    highlight: (code, lang) => {
        if (lang) {
            const highlighted = highlightFile([code], `.${lang}`);
            return highlighted;
        }
        return code;
    }
});
```

## Files Changed

| File | Change |
|------|--------|
| `web/package.json` | Add `marked` dependency |
| `web/src/lib/FileViewer.svelte` | Add markdown detection, toggle button, rendering logic, styles |
| `web/src/lib/profile.ts` | Add `markdownFormat` to Profile interface |

## Testing

**Test cases**:

1. **Toggle functionality**:
   - Open a `.md` file
   - Verify "Format" button appears
   - Click button, verify markdown renders
   - Click again, verify raw markdown shows

2. **Markdown features**:
   - Headers: All levels render correctly
   - Lists: Ordered, unordered, nested
   - Links: Clickable, correct color
   - Code: Inline and block, syntax highlighted
   - Tables: Proper borders and striping
   - Blockquotes: Left border, muted

3. **Edge cases**:
   - Empty markdown file
   - Markdown with HTML (should be sanitized)
   - Very long markdown files
   - Markdown with images

4. **Preference persistence**:
   - Toggle format ON
   - Refresh page
   - Open another markdown file
   - Verify format is still ON

5. **Non-markdown files**:
   - Open `.txt`, `.js`, `.rs` files
   - Verify "Format" button does NOT appear

## Security Considerations

**XSS Prevention**:
- `marked` sanitizes HTML by default
- Use `{@html}` only for marked output
- Don't allow arbitrary HTML injection

**Content Security Policy**:
- Inline styles in markdown should be stripped
- External image loading (consider allow/block)

## Future Enhancements

1. **Mermaid diagrams** - Render ```mermaid code blocks
2. **LaTeX math** - Render math expressions
3. **Table of contents** - Auto-generate from headers
4. **Export** - Download rendered markdown as PDF/HTML
5. **Edit mode** - Live preview while editing
6. **Frontmatter** - Display YAML frontmatter nicely

## Summary

This feature adds markdown rendering to the file viewer with:
- ✅ Simple toggle button (like Wrap)
- ✅ Client-side rendering (no server changes)
- ✅ Full GFM support (tables, strikethrough, etc.)
- ✅ Syntax highlighting in code blocks
- ✅ Theme-aware styling
- ✅ Preference persistence
- ✅ ~100 lines of code

Makes documentation and markdown files much more readable while maintaining the ability to view raw source.
