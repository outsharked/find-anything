# Troubleshooting

[← Manual home](README.md)

---

## find-watch crashes immediately

**Symptom:** `find-watch` exits immediately after starting, often with an error about inotify limits or missing extractors.

**Check the logs:**

```sh
journalctl --user -u find-watch -n 50
# or for a system service:
journalctl -u find-watch -n 50
```

**Inotify watch limit exceeded (Linux):**

```
Error: Os { code: 28, kind: StorageFilledUp, message: "No space left on device" }
```

Linux limits the number of filesystem watches per user. Raise the limit:

```sh
# Temporary (until next reboot)
sudo sysctl fs.inotify.max_user_watches=524288

# Permanent
echo fs.inotify.max_user_watches=524288 | sudo tee /etc/sysctl.d/50-inotify.conf
sudo sysctl -p /etc/sysctl.d/50-inotify.conf
```

**Missing extractor binaries:**

```
Error: extractor not found: find-extract-pdf
```

The `find-extract-*` binaries must be in the same directory as `find-watch`, on `PATH`, or their location set via `watch.extractor_dir` in `client.toml`. Verify:

```sh
which find-extract-text
ls $(dirname $(which find-watch))/find-extract-*
```

**Cannot connect to server:**

```
Error: connection refused
```

Check that `find-server` is running and that the `server.url` in `client.toml` is correct. Test with:

```sh
find-admin check
```

---

## Files not appearing in search

**Check that the source is configured correctly:**

```sh
find-admin sources
```

The source name shown must match the `name` in your `[[sources]]` config.

**Check that the file was scanned:**

```sh
find-scan --dry-run /path/to/file
```

If `--dry-run` shows the file as "would index", run `find-scan /path/to/file` to index it immediately.

**Check for exclusion rules:**

The file may be excluded by:

- `scan.exclude` glob patterns in `client.toml`
- A `.noindex` file in a parent directory
- `scan.include_hidden = false` (the default) — dot-files and dot-directories are skipped
- `scan.max_content_size_mb` — files above this limit are skipped

**Check for extraction errors:**

```sh
find-admin status --json | jq '.sources[] | select(.name=="your-source") | .errors'
```

Or check **Settings → Errors** in the web UI.

**The server may still be processing:**

After `find-scan` completes, the server processes the batch asynchronously. This usually takes under a second but can be longer for very large batches. Check the worker status:

```sh
find-admin status --json | jq '.worker_status'
```

Wait for it to show `"idle"` before concluding a file is missing.

---

## Synology NAS

Running `find-watch` on a Synology NAS requires a few adjustments:

**inotify is supported but limited:**

Synology DSM supports inotify, but the default watch limit is very low. Raise it via SSH:

```sh
sudo sysctl -w fs.inotify.max_user_watches=65536
```

To make this permanent, add it to `/etc/sysctl.conf` or a DSM startup task.

**Shared folders:**

Index the real path of shared folders (e.g. `/volume1/documents`) rather than a symlink. If `scan.follow_symlinks = false` (the default), symlinks inside source paths are not followed.

**Running as a service:**

Synology does not use systemd. Options:

- Use a **Task Scheduler** task (DSM → Control Panel → Task Scheduler) to start `find-watch` at boot.
- Use a third-party process manager (e.g. `supervisord` if available).
- Use the Docker package to run `find-server` and a container-based watcher.

**ARM architecture:**

Synology devices typically use ARM CPUs. Download the ARM build from GitHub Releases, or cross-compile:

```sh
rustup target add armv7-unknown-linux-musleabihf
cargo build --release --target armv7-unknown-linux-musleabihf
```

---

## High memory usage

**`find-scan` memory spike during a scan:**

`find-scan` holds extracted content in memory before sending it to the server. For very large files (near `max_content_size_mb`) or large archives, this can be significant. If memory is constrained:

- Lower `scan.max_content_size_mb` (default: 10)
- Lower `scan.archives.max_7z_solid_block_mb` (default: 256)
- Lower `scan.archives.max_temp_file_mb` (default: 500)

**`find-server` memory growth:**

The server caches ZIP archive handles and frequently accessed chunks in memory. This is expected behavior and generally bounded. If the server consumes more memory than expected:

- Check for a stuck inbox: `find-admin status --json | jq '.worker_status'`
- Restart the server to clear caches: `systemctl restart find-server`

---

## Slow search

**Check `fts_candidate_limit` in `server.toml`:**

Higher values improve recall but increase CPU per query. Lower them if queries feel slow:

```toml
[search]
fts_candidate_limit = 500   # default: 2000
```

**Large index:**

SQLite FTS5 is fast but performance degrades with very large indexes (100M+ rows). Splitting a very large source into multiple smaller sources can help.

**Network latency:**

The web UI makes HTTP requests to the server for each search. If the server is on a remote machine, latency affects search feel. The server has no built-in TLS — if you need it over the internet, put it behind a reverse proxy with HTTPS and consider running it on a local network instead.

**Regex mode is slowest:**

Regex queries cannot use the FTS5 index efficiently and scan all rows. Avoid `--mode regex` for large indexes unless necessary, or narrow results first with a source filter.

---

[← Running as a service](08-services.md)
