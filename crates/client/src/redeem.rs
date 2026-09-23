//! Writing a redeemed access token into `client.toml` (plan 094).
//!
//! Kept as a pure text transformation so an existing config's comments,
//! ordering and `[[sources]]` blocks survive untouched.

use anyhow::{bail, Result};

fn quote(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// True if `line` is a TOML table header (`[x]` / `[[x]]`), ignoring indentation.
fn is_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// Set `key = "value"` inside the `[server]` table of `existing` (or create the
/// table / the whole file). Returns the new file contents.
fn set_server_key(existing: &str, key: &str, value: &str) -> String {
    let line_for = |k: &str, v: &str| format!("{k} = {}", quote(v));
    let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();

    let Some(start) = lines.iter().position(|l| l.trim() == "[server]") else {
        let mut out = existing.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("[server]\n{}\n", line_for(key, value)));
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| is_header(l))
        .map_or(lines.len(), |i| start + 1 + i);

    let is_key_line = |l: &str| {
        let t = l.trim_start();
        t.strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    if let Some(i) = (start + 1..end).find(|&i| is_key_line(&lines[i])) {
        lines[i] = line_for(key, value);
    } else {
        // Insert after the last non-blank line of the table.
        let mut at = end;
        while at > start + 1 && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        lines.insert(at, line_for(key, value));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Produce the new `client.toml` contents with `token` (and `url`, if given)
/// set in `[server]`. `existing` is `None` when the file does not exist yet,
/// in which case `url` is required.
pub fn apply_token(existing: Option<&str>, url: Option<&str>, token: &str) -> Result<String> {
    if existing.is_none() && url.is_none() {
        bail!("no existing client.toml — pass --url <server url>");
    }
    let mut text = existing.unwrap_or("").to_string();
    if let Some(url) = url {
        text = set_server_key(&text, "url", url);
    }
    text = set_server_key(&text, "token", token);

    // Guard against exotic layouts (inline tables, dotted keys) where the
    // line-based edit would not have taken effect.
    let parsed: toml::Value = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("resulting config is not valid TOML: {e}"))?;
    let got = parsed.get("server").and_then(|s| s.get("token")).and_then(|t| t.as_str());
    if got != Some(token) {
        bail!("could not update [server] token automatically — set it manually");
    }
    Ok(text)
}

/// Read `server.url` out of an existing config, if present.
pub fn existing_url(existing: &str) -> Option<String> {
    toml::from_str::<toml::Value>(existing)
        .ok()?
        .get("server")?
        .get("url")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_fresh_config() {
        let out = apply_token(None, Some("http://h:8080"), "fa_x").unwrap();
        assert_eq!(out, "[server]\nurl = \"http://h:8080\"\ntoken = \"fa_x\"\n");
    }

    #[test]
    fn fresh_config_requires_url() {
        assert!(apply_token(None, None, "fa_x").is_err());
    }

    #[test]
    fn replaces_token_and_preserves_everything_else() {
        let existing = "# my config\n[server]\nurl   = \"http://h\"\ntoken = \"old\"  # comment\n\n[[sources]]\nname = \"a\"\npath = \"/a\"\n";
        let out = apply_token(Some(existing), None, "fa_new").unwrap();
        assert_eq!(
            out,
            "# my config\n[server]\nurl   = \"http://h\"\ntoken = \"fa_new\"\n\n[[sources]]\nname = \"a\"\npath = \"/a\"\n"
        );
    }

    #[test]
    fn adds_token_when_missing_from_server_table() {
        let existing = "[server]\nurl = \"http://h\"\n\n[scan]\nx = 1\n";
        let out = apply_token(Some(existing), None, "fa_t").unwrap();
        assert_eq!(out, "[server]\nurl = \"http://h\"\ntoken = \"fa_t\"\n\n[scan]\nx = 1\n");
    }

    #[test]
    fn does_not_touch_token_keys_in_other_tables() {
        let existing = "[server]\nurl = \"u\"\ntoken = \"old\"\n\n[tray]\ntoken = \"keep\"\n";
        let out = apply_token(Some(existing), None, "fa_t").unwrap();
        assert!(out.contains("[tray]\ntoken = \"keep\""));
        assert!(out.contains("[server]\nurl = \"u\"\ntoken = \"fa_t\""));
    }

    #[test]
    fn appends_server_table_when_absent() {
        let out = apply_token(Some("[scan]\nx = 1\n"), Some("http://h"), "fa_t").unwrap();
        assert!(out.starts_with("[scan]\nx = 1\n"));
        assert!(out.ends_with("[server]\nurl = \"http://h\"\ntoken = \"fa_t\"\n"));
    }

    #[test]
    fn url_override_updates_existing_url() {
        let existing = "[server]\nurl = \"http://old\"\ntoken = \"t\"\n";
        let out = apply_token(Some(existing), Some("http://new"), "fa_t").unwrap();
        assert_eq!(existing_url(&out).as_deref(), Some("http://new"));
    }

    #[test]
    fn special_characters_are_escaped() {
        let out = apply_token(None, Some("http://h"), "a\"b\\c").unwrap();
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["server"]["token"].as_str(), Some("a\"b\\c"));
    }

    #[test]
    fn inline_table_layout_is_rejected_not_corrupted() {
        let existing = "server = { url = \"http://h\", token = \"old\" }\n";
        assert!(apply_token(Some(existing), None, "fa_t").is_err());
    }
}
