---
description: Update CHANGELOG [Unreleased] with a summary of changes and commit
---

You are helping commit changes to the find-anything project. Follow these steps exactly:

**Step 1 — Understand the changes**

Run these in parallel:
- `git diff HEAD` (staged + unstaged changes)
- `git status` (to see what files are modified/new)
- Read `CHANGELOG.md` to see the current `[Unreleased]` section

**Step 2 — Update CHANGELOG.md**

Add a summary of the changes to the `## [Unreleased]` section in `CHANGELOG.md`. Follow the existing style:
- Use `### Added`, `### Fixed`, `### Changed` subsections as appropriate (only include sections that apply)
- Each entry is a bullet: `- **Subject** — description of what changed and why`
- Be concise but precise; focus on user-visible behaviour and technical correctness
- Do not duplicate entries already in `[Unreleased]`; append or extend as needed
- Do not create a new versioned section — changes go under `[Unreleased]` only

**Step 3 — Stage and commit**

Stage all modified tracked files (do not use `git add -A`; add files by name). Include `CHANGELOG.md`.

Write a commit message in the style of recent commits (`git log --oneline -10`): short imperative subject line, no body needed unless the change is complex.

End the commit message with:
```
Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
```

Use a HEREDOC for the commit message to preserve formatting.

**Step 2b — Check `MIN_CLIENT_VERSION` (API breaking changes only)**

After running clippy (or if no `.rs` files were changed), inspect the diff for any breaking API changes:
- Removed or renamed HTTP endpoints
- New required fields in request/response bodies
- Removed or renamed response fields
- Changed endpoint semantics

If any breaking changes are present, update `MIN_CLIENT_VERSION` in `crates/common/src/api.rs` to match the current package version (from any `Cargo.toml`). If no breaking changes, leave it unchanged.

**Important rules:**
- Never `git push` — only commit locally
- Never use `--no-verify`
- If `$ARGUMENTS` is provided, treat it as a hint or override for the commit message subject
- Run `clippy` first if any `.rs` files are modified: `mise run clippy`. Fix any warnings before committing.
