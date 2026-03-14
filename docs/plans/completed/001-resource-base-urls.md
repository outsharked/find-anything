# 001 - Resource Base URLs

## Overview

Add support for base URLs in source configuration, allowing clients to specify how indexed resources can be accessed via URL. This enables hyperlinking from search results to the actual files, whether they're on a local filesystem, network share, web server, or version control system.

## Use Cases

1. **Local file access**: `file:///mnt/share/code` + `src/main.rs` → `file:///mnt/share/code/src/main.rs`
2. **Network shares**: `smb://server/share` + `docs/readme.md` → `smb://server/share/docs/readme.md`
3. **GitHub repos**: `https://github.com/owner/repo/blob/main` + `src/lib.rs` → `https://github.com/owner/repo/blob/main/src/lib.rs`
4. **Web servers**: `https://docs.example.com` + `api/reference.html` → `https://docs.example.com/api/reference.html`

## Design Decisions

### Configuration Location

Add `base_url` as an **optional** field on each source:
```toml
[[sources]]
name = "code"
paths = ["/home/user/code"]
base_url = "file:///home/user/code"  # Optional
```

**Rationale**:
- Per-source configuration allows different sources to have different access methods
- Optional field maintains backward compatibility
- Source-level makes sense since different sources may be on different shares/servers

### Storage

Store base_url in the source metadata:
- Add `base_url` column to `meta` table in each source database
- Return in search results so web UI can construct links
- Empty/null base_url means no hyperlinking available for that source

### Path Joining

Use simple string concatenation with `/` separator:
- Normalize: ensure base_url doesn't end with `/`, indexed paths don't start with `/`
- Join: `base_url + "/" + relative_path`
- Handles: `file://`, `http://`, `https://`, `smb://`, custom protocols

## Implementation

### Phase 1: Configuration and Storage

**Files to modify:**

1. `crates/common/src/config.rs`
   - Add `base_url: Option<String>` to `SourceConfig`

2. `crates/server/src/db.rs`
   - Add base_url to source metadata storage
   - Return base_url in source queries

3. `crates/client/src/api.rs`
   - Include base_url when registering/updating source

4. `examples/client.toml`
   - Add example with base_url

### Phase 2: API Response

**Files to modify:**

5. `crates/common/src/api.rs`
   - Add `base_url: Option<String>` to `SearchResult` struct
   - Include in serialization

6. `crates/server/src/routes/search.rs`
   - Include base_url in search results
   - Construct full URL if base_url exists: `base_url + "/" + path`

### Phase 3: Web UI Integration

**Files to modify:**

7. `web/src/lib/SearchResult.svelte`
   - Display clickable link if `base_url` exists
   - Show icon indicating link type (file://, http://, etc.)

8. `web/src/lib/FileViewer.svelte`
   - Add "Open in..." button if base_url exists

## Database Schema Changes

```sql
-- Add to meta table keys:
-- 'base_url' → optional URL prefix for this source
```

No schema migration needed - just add a new key to the existing meta table.

## API Changes

### Search Response (Enhanced)

```json
{
  "results": [
    {
      "source": "code",
      "path": "src/main.rs",
      "line_number": 42,
      "content": "...",
      "score": 0.95,
      "resource_url": "file:///home/user/code/src/main.rs"  // NEW
    }
  ]
}
```

**Backward Compatibility**: `resource_url` is optional, only included if base_url is configured.

## Example Configuration

```toml
[server]
url = "http://localhost:8765"
token = "secret"

[[sources]]
name = "local-code"
paths = ["/home/user/code"]
base_url = "file:///home/user/code"

[[sources]]
name = "github-repo"
paths = ["/tmp/cloned-repo"]
base_url = "https://github.com/owner/repo/blob/main"

[[sources]]
name = "docs-site"
paths = ["/var/www/docs"]
base_url = "https://docs.example.com"

[scan]
exclude = ["**/.git/**", "**/node_modules/**"]
```

## Testing

1. **Configuration parsing**: Verify base_url is correctly parsed and optional
2. **Source registration**: Confirm base_url is stored in database
3. **Search results**: Verify resource_url is correctly constructed
4. **Path normalization**: Test various base_url formats (trailing slash, no trailing slash)
5. **Missing base_url**: Confirm search works when base_url is absent
6. **Web UI**: Verify links are clickable and correct

## Breaking Changes

None. This is a purely additive feature:
- Existing configs without `base_url` continue to work
- Existing API responses add an optional field
- Web UI gracefully handles missing `resource_url`

## Future Enhancements

1. Support for line number anchors: `resource_url + "#L42"`
2. Template-based URL construction: `{base_url}/{path}#{line}`
3. Protocol-specific handling (e.g., VS Code URIs: `vscode://file/{path}:{line}`)
4. Archive entry URLs: `{base_url}/{archive_path}#!/{entry_path}`
