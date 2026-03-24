# TODO

Stream of consciousness for bugfixes and features approaching V1

## Bugs/Enhancements

[ ] CHORE: Optimize "tree" queries- it makes one query per level of the tree view
[ ] FEAT: Duplicate management view?
[ ] BUG: App ID is not persisted, each rebuild makes it not show up in pinned taskbar items
[ ] BUG: Windows shows "stop watcher" before connecting to service, it should know right away status of watcher
[ ] CHORE: Server install should also be able to chain client install
[ ] CHORE: Include **/AppData/**, **/bin/**, **/obj/**, **/\*.log by default, "**/pnpm/\*\*", vscode-remote-wsl
[ ] FEAT: admin email for: failed items (daily status); low disk space; overall statistics. "every day" or only warning
[ ] FEAT: Guard against low disk space
[ ] FEAT: Allow non-zip archive members to be streamable, if they are below a configrable threshold in size
[ ] Evaluate scoring algorithm: recent dates score higher; but if we use a better strategy for storing content, then 
use the content
[ ] BUG find-admin inbox-pause should also pause archiving. And it doesn't log except the API
[ ] FEAT format times as 4m22s
[ ] CHORE: frontent ux tests; frontend code quality. LOooks like lots of css duplication among other issues. Lots of components reimplemented everywhere
[ ] FEAT: When [sources.nas-data] path = "/mnt/nas-data" is missing from server, show a clear inline error message or fallback UI in the client
[ ] BUG: Fetching existing file list seems to not be targeted to a particular tree (e.g. find-scan /path/to/dir)
[ ] FEAT: make "find-admin inbox" have subcommands
[ ] FEAT: Run compaction scan only once a day, or on demand
[ ] FEAT: Give higher priority to more recently modified documents
[ ] FEAT: UX to retry failed scans (admin page)
[ ] FEAT: Write logs from extractors as structured json, optionally, for tool use error reporting: "As for why the member path isn't shown: the relay attaches file=Blog.zip because that's all the client knows at that point — the archive extractor subprocess's entire stderr is collected and relayed as a batch with the outer file as context. Symphonia fires inside that subprocess against a temp file, with no way to surface which archive member it was probing. There's no good fix for that without restructuring how the archive extractor emits its logs (e.g. writing structured JSON stderr instead of plain text)." (Symphonia errors don't show full path to file in client scanner)

### Code Quality

[ ] CHORE: address clippy too_many_arguments issues
[ ] CHORE: Code quality: avoid optional typescript parameters; default values should be set at as high a level as possible and propagated conclusively. Add to claude.md
[ ] CHORE: Frontent factoring: stuff like     "showOriginal = fileKind === 'image' || fileKind === 'video' || fileKind === 'audio' || (fileKind === 'pdf' && !isEncrypted && preferOriginal); " -- logic should be centralized. 
 - use general purpose functions to return type, not isMarkdown, isRTF etc
[ ] CHORE: refactor front end to split svg assets out into files, etc

### Major features/Integration

[ ] FEAT (planned): Add providing uname/password for share to config for each source
[ ] FEAT (PLANNED) metrics - log start/end time of critical proceses to an external service - Grafana LGTM?
[ ] FEAT (PLANNED): allow adding file extension -> content type mappers. Extract all hardcoded into config
[ ] WinGet
[ ] Use Signpath code signing
[ ] Stand up demo on fly.io

### Completed Items

[x] BUG don't show "results" button when we deeplinked to something
[x] BUG: Don't scroll metadata below images in detail view; just have it directly in view
[x] BUG: Searching on ".png" doesn't work is that improvable
[x] FEAT: Custom protocol handler (chrome) to allow exploring the file location. Requires client config for roots.
[x] FEAT: Render SVG by default
[x] FEAT: Mobile friendly layou
[x] BUG: pictures/2014/Jamie Phone 2014/20140410_074302.jpg - no metadata available - why?
[x] BUG: back arrow browser navigation doesn't work at all
[x] BUG: backups/FromMomMac/FromMomMac.zip::FromMomMac/Library/Mail/V7/079E825E-CEF8-46FA-813A-F63AAB5350AC/[Gmail].mbox/All Mail.mbox/95F6C28D-53E9-4B54-BF8E-0B058ABFAFAF/Data/3/4/5/Attachments/543053/1/Christmas Letter 2016.rtfd/Monhegan Lighthouse Dory 2005.tif can't be shown. [Note: this is a jpeg file with a tiff extension; added magic byte detection]
[x] Regular download button should be available in archives too
[x] FEAT: make chevron bar wider
[x] FEAT "split/extracted" doesn't make sense with slideeout - just remove button.
[x] FEAT: Add image zoom/pan controls to normal detail view
[x] FEAT: Can collapse tree even when selected
[x] archive/unfiled/OpenMULE-Win32.zip::resources/action.wav shows a duplicate. But when you click on it, it doesn't have a dup link back to the original.
[x] CHORE: Unit test for handling of retrieval of duplicated items in index
[x] FEAT: In image detail split view, should be able to move divider/resize window (implemented as slideout drawer)
[x] Allow playing audio files
[x] FEAT: Use human readable numbers Mar 17 15:01:26 findanything find-server[312010]:  INFO compaction scan: 2553268401/4119151881 bytes orphaned (62.0%) in 632.3s
[x] BUG: Is RAR content indexed? Do we compute hashes of all files? For example 1992-04-05 - The Fox Theatre - Boulder, CO part 1 - Copy.rar::I 01 Llama.mp3 is a duplicate of 1992-04-05 - The Fox Theatre - Boulder, CO part 1 - Copy (2).rar::I 01 Llama.mp3 but not marked as so
[x] CHORE: Analysis of testing gaps
[x] CHORE: Inline extraction of safe file types instead of making subporocesses
[x] FEAT: Add query syntax "type: image" with intellisense (note: we implemented this but not intellisense)
[x] FEAT: Add query syntax "path: /backups/" with intellisense (note: we implemented this but not intellisense)
[x] FEAT: use video player for video files
[x] FEAT: UX is bad when showing full context for large text documents. Use some kind of scroll pattern to not render the whole thing onscreen
[x] FEAT: Create short URL to link
[x] BUG: "Content has changed. Reload" on a detail page doesn't clear when you hit reload, only hard refresh
[x] FEAT (PLANNED): allow adding extra tools (similar to formatters) for archive extraction. Extract hardcoded into default config
[x] BUG: Dynamic width of ctrl+p is an issue - it should stay fixed size. Maybe we should get rid of this and have 
    file:fox theater (look at sourcegraph)
[x] Look at debug logs during e.g. a simple delete - seems to do a lot of stuff it doesn't need to, e.g. passing off to archving. We should pre-filter the gz file - if only delete/small adds, no need to go to archiving. Check szie of text content, file types. (Note: we no longer delete from archives, it's garbage collected now)
[x] FEAT: find-reindex -- can this only operate against previously-not-reindexed content?
[x] CHORE: Look at node-tar:fixtures.tgz -- this a great stress test. Copy fixtures.tgz
[x] FEAT: add -f/--follow to find-recent/ /btw
[x] BUG: "WARN find_server::routes::search: search source error: fts5: syntax error near ".": Error code 1: SQL logic error"
[x] BUG: Hand pointer shows when hovering over :line in search results page when there are no "next/prev" line arrows
[x] FEAT: Arrow Up/Down while an item is selected in left nav should move to next/previous item
[x] FEAT: find-watch should buffer changes for some configurable # of seconds before sending the update
[x] FEAT: When files added/removed, tree view does not update. (sse)
[x] CHORE: Integration tests
[x] CHORE: find-scan and find-watch should follow all the same rules and configuration during their walk of the tree. The only thing they do differenltly is register a watcher, or call the indexer. Can we reuse this code, and have each client pass in an callback that gets called for each file that is accepted by the walk, and can then either register a watch or do the indexing?
[x] BUG: Create a file, then rename it, it has a duplicate pointing to original
[x] BUG: Don't emit this like a log, this should be user-facing: ❯ find-admin status > 2026-03-12T20:32:11.451925Z  WARN find_common::config: unknown config key: sources.0.base_url
[x] BUG: Duplicate shows many copies : pictures/2020/takeout-20200507T101645Z-001.zip::Takeout/Google Photos/Naomi_s iPhone 4s/IMG_0576.JPG
[x] BUG: When typing in a search query while detail page is showing (left nav selected), after it reverts to search results, focus is lost from search bar
[x] What does this mean? No files actually were indexed - find_scan::scan: processed 32312 files (22283 unchanged, 10029 new) so far... -find_scan::scan: scan complete — 10029 indexed (10029 new, 0 modified, 0 upgraded), 77402 unchanged, 0 deleted
[x] BUG: Searches are case sensitiive
[x] FEAT: Add an X to search box
[x] FEAT: emit version with each tool --version
[x] FEAT: Show full path when hovering in search results view
[x] BUG: File path in search detail view cuts off on right, should go multiline
[x] FEAT: find-admin --watch feature
[x] FEAT: UX - sources should have no triangle. When selected, they become bold. Font slightly bigger than tree font.
[x] BUG: Address locked file handling
[x] BUG: Ensure that we don't reindex something with a datestamp older than file was actually indexed in our database
[x] FEAT:In this log, don't emit pieces that have 0 files changed: 2026-03-07T02:49:24.771634Z INFO find_scan::scan: processed 5724 files (5723 unchanged, 1 new, 0 modified, 0 upgraded) so far, 0 in current batch...
[x] CHORE: Remove Base URL feature from config
[x] BUG: Installer on windows doens't really work, there is no service created.
[x] BUG: Windows - left-clcking shows only filename - need path. UX is weird, need a scrollbar. Need time,.
[x] FEAT: If a path is truncated, the "copy path" icon should still be visible flushed right. So if the path fits onscreen, the copyt icon should be to the right of the end ofit, but if the path is truncated the icon should remain onscreen flished right, after the truncated path.
[x] BUG: The tree view should scroll indendently of search results
[x] FEAT: Add "include" also to [scan] for clients. So we can make .index that only includes certain things
[x] BUG: UX - our UX update about focus results in page being unclickable except for scrollbars
[x] FEAT: `tool --version` should also show (release) or commit hash, same as in web UX
[x] FEAT: Add link to github repo in Find Anything watcher
[x] FEAT: default windows config should include commented-out options
[x] FEAT: Self-update in about
[x] FEAT: Natural language search terms -- start with chrono-node for natural language date queries 1. should be able to type "automation or foo" in the last 2 days 2. should return a search construct that is compatible 3. need to add 'or' searches, compose or/and/etc 4. need config for llm - openai? local?
[x] CHORE: Remove baseurl & baseurl overrides in ux
[x] FEAT: Fix the search bar at the top of the screen
[x] FEAT: Copy to clipoboard icon should be immediately at end fo filename, and doesn't work, and should show tooltip "copied" that disappears in 2 seconds
[x] FEAT: When multiple results in a file, only show one entry, but show multiple line numbers :123,456 and make them clickable to change context.
[x] FEAT: Show build hash in about
[x] BUG: a file with 3 lines, and a context window of +/-2 shows with no context
[x] FEAT: find-scan should allow a directory as argument, not just a file
[x] FEAT: Show timestamp on search result
[x] FEAT: When clicking a PDF from navigator, show it immediately, only show extracted text first when navigating from search resuilt
[x] FEAT: Limit results to mtime range
[x] FEAT: Add "exclude_extra = []' to default config

#### Ecosystem
