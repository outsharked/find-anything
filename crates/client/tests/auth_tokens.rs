//! End-to-end: `find-admin` invite → redeem → scoped token in client.toml (plan 094).

mod helpers;
use helpers::{TestServer, TEST_TOKEN};

use find_client::api::ApiClient;
use std::path::Path;

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Run the built `find-admin` binary. Blocking, so it runs off the runtime
/// threads (the in-process test server needs them).
async fn find_admin(config: &Path, args: &[&str]) -> Out {
    let config = config.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_find-admin"))
            .arg("--config")
            .arg(&config)
            .args(&args)
            .env_remove("FIND_ANYTHING_CONFIG")
            .output()
            .expect("run find-admin");
        Out {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    })
    .await
    .unwrap()
}

fn write_admin_config(dir: &Path, url: &str) -> std::path::PathBuf {
    let p = dir.join("admin.toml");
    std::fs::write(&p, format!("[server]\nurl = \"{url}\"\ntoken = \"{TEST_TOKEN}\"\n")).unwrap();
    p
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invite_redeem_flow_produces_scoped_token() {
    let srv = TestServer::spawn().await;
    let dir = tempfile::TempDir::new().unwrap();
    let admin_cfg = write_admin_config(dir.path(), &srv.base_url);

    // 1. Admin mints an update-index invite.
    let out = find_admin(
        &admin_cfg,
        &["--json", "invite", "create", "--name", "scanner1", "--scope", "update-index", "--ttl", "5m"],
    )
    .await;
    assert!(out.ok, "invite create failed: {}", out.stderr);
    let code = serde_json::from_str::<serde_json::Value>(&out.stdout).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    // 2. A fresh machine (no client.toml yet) redeems it.
    let client_cfg = dir.path().join("nested").join("client.toml");
    let out = find_admin(&client_cfg, &["redeem", &code, "--url", &srv.base_url]).await;
    assert!(out.ok, "redeem failed: {}", out.stderr);
    assert!(out.stdout.contains("scanner1"), "{}", out.stdout);

    let written = std::fs::read_to_string(&client_cfg).unwrap();
    let parsed: toml::Value = toml::from_str(&written).unwrap();
    let token = parsed["server"]["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("fa_"));
    assert_eq!(parsed["server"]["url"].as_str().unwrap(), srv.base_url);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&client_cfg).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config holding a token must be owner-only");
    }

    // 3. The saved token has update-index scope: reads and indexing work,
    //    admin does not.
    let api = ApiClient::new(&srv.base_url, &token);
    api.get_sources().await.expect("read allowed");
    api.inbox_status().await.expect_err("admin route must be refused");
    let out = find_admin(&client_cfg, &["token", "list"]).await;
    assert!(!out.ok, "non-admin token must not list tokens");

    // 4. The same invite cannot be redeemed twice.
    let other_cfg = dir.path().join("other.toml");
    let out = find_admin(&other_cfg, &["redeem", &code, "--url", &srv.base_url]).await;
    assert!(!out.ok);
    assert!(out.stderr.contains("invalid, expired or already-used"), "{}", out.stderr);
    assert!(!other_cfg.exists(), "failed redeem must not create a config");

    // 5. Admin lists (no secrets) and revokes; the token stops working at once.
    let out = find_admin(&admin_cfg, &["--json", "token", "list"]).await;
    assert!(out.ok, "{}", out.stderr);
    assert!(out.stdout.contains("scanner1"));
    assert!(!out.stdout.contains(&token), "token list must not reveal token values");

    let out = find_admin(&admin_cfg, &["token", "revoke", "scanner1"]).await;
    assert!(out.ok, "{}", out.stderr);
    api.get_sources().await.expect_err("revoked token must be rejected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redeem_preserves_existing_config_and_reuses_its_url() {
    let srv = TestServer::spawn().await;
    let dir = tempfile::TempDir::new().unwrap();
    let admin_cfg = write_admin_config(dir.path(), &srv.base_url);

    let out = find_admin(
        &admin_cfg,
        &["--json", "invite", "create", "--name", "laptop", "--scope", "read"],
    )
    .await;
    let code = serde_json::from_str::<serde_json::Value>(&out.stdout).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    let cfg = dir.path().join("client.toml");
    std::fs::write(
        &cfg,
        format!(
            "# keep me\n[server]\nurl = \"{}\"\ntoken = \"old\"\n\n[[sources]]\nname = \"docs\"\npath = \"/tmp\"\n",
            srv.base_url
        ),
    )
    .unwrap();
    // No --url: taken from the existing config.
    let out = find_admin(&cfg, &["redeem", &code]).await;
    assert!(out.ok, "{}", out.stderr);
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(text.starts_with("# keep me\n"));
    assert!(text.contains("[[sources]]\nname = \"docs\""));
    assert!(!text.contains("\"old\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invite_list_and_revoke_via_cli() {
    let srv = TestServer::spawn().await;
    let dir = tempfile::TempDir::new().unwrap();
    let admin_cfg = write_admin_config(dir.path(), &srv.base_url);

    let out = find_admin(&admin_cfg, &["invite", "create", "--name", "x", "--scope", "read"]).await;
    assert!(out.ok, "{}", out.stderr);
    assert!(out.stdout.contains("find-admin redeem"), "prints the redeem command: {}", out.stdout);

    let out = find_admin(&admin_cfg, &["--json", "invite", "list"]).await;
    let list: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let id = list[0]["id"].as_i64().unwrap();

    let out = find_admin(&admin_cfg, &["invite", "revoke", &id.to_string()]).await;
    assert!(out.ok, "{}", out.stderr);
    let out = find_admin(&admin_cfg, &["invite", "revoke", &id.to_string()]).await;
    assert!(!out.ok, "revoking twice must fail");

    // Duplicate names are refused with a readable message.
    find_admin(&admin_cfg, &["invite", "create", "--name", "dup", "--scope", "read"]).await;
    let out = find_admin(&admin_cfg, &["invite", "create", "--name", "dup", "--scope", "read"]).await;
    assert!(!out.ok);
    assert!(out.stderr.contains("already exists"), "{}", out.stderr);
}
