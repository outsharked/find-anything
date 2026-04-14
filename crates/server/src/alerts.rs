/// Outgoing alert email notifications.
///
/// Sends alerts via SMTP using the `[alerts]` configuration block.
/// No email is sent when `smtp_host` is absent from the config.
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};

use find_common::config::{AlertsConfig, SmtpEncryption};

/// Send an alert email when the inbox worker trips the timeout circuit breaker.
///
/// Spawns a blocking task and returns immediately — alert delivery does not
/// block the calling async context.  Errors are logged but not propagated.
pub fn send_inbox_paused_alert(cfg: &AlertsConfig, consecutive_count: u32, timeout_secs: u64) {
    let (Some(to), Some(host), Some(from)) =
        (&cfg.admin_email, &cfg.smtp_host, &cfg.smtp_from)
    else {
        if cfg.smtp_host.is_some() {
            tracing::warn!(
                "SMTP host is configured but admin_email or smtp_from is missing; \
                 skipping inbox-paused alert"
            );
        }
        return;
    };

    let cfg = cfg.clone();
    let to = to.clone();
    let from = from.clone();
    let host = host.clone();

    tokio::task::spawn_blocking(move || {
        if let Err(e) =
            send_smtp(&cfg, &host, &to, &from, consecutive_count, timeout_secs)
        {
            tracing::error!("Failed to send inbox-paused alert email to {to}: {e:#}");
        } else {
            tracing::info!("Inbox-paused alert email sent to {to}");
        }
    });
}

fn send_smtp(
    cfg: &AlertsConfig,
    host: &str,
    to: &str,
    from: &str,
    count: u32,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let hostname = read_hostname();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    let subject = format!(
        "find-anything inbox worker paused ({count} consecutive timeouts)"
    );
    let body = format!(
        "The find-anything inbox worker on {hostname} has been automatically paused\n\
         after {count} consecutive request processing timeouts ({timeout_secs}s each).\n\
         \n\
         This typically indicates a stuck database write lock or internal deadlock.\n\
         Time of last timeout: {now}\n\
         \n\
         No further requests will be processed until the worker is resumed manually.\n\
         \n\
         To resume:\n\
           find-admin inbox resume\n\
         \n\
         Or via the API:\n\
           POST /api/v1/admin/inbox/resume\n\
         \n\
         To investigate, check the inbox/failed/ directory in the server data directory.\n"
    );

    let email = Message::builder()
        .from(from.parse()?)
        .to(to.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body)?;

    let creds = match (&cfg.smtp_username, &cfg.smtp_password) {
        (Some(u), Some(p)) => Some(Credentials::new(u.clone(), p.clone())),
        _ => None,
    };

    match cfg.smtp_encryption {
        SmtpEncryption::Tls => {
            let mut b = SmtpTransport::relay(host)?.port(cfg.smtp_port);
            if let Some(c) = creds {
                b = b.credentials(c);
            }
            b.build().send(&email)?;
        }
        SmtpEncryption::Starttls => {
            let mut b = SmtpTransport::starttls_relay(host)?.port(cfg.smtp_port);
            if let Some(c) = creds {
                b = b.credentials(c);
            }
            b.build().send(&email)?;
        }
        SmtpEncryption::None => {
            let mut b = SmtpTransport::builder_dangerous(host).port(cfg.smtp_port);
            if let Some(c) = creds {
                b = b.credentials(c);
            }
            b.build().send(&email)?;
        }
    }

    Ok(())
}

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
