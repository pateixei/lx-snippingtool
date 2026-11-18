//! Post-copy desktop notifications via `ashpd::desktop::notification` that spawn
//! `lx-snippingtool --editor <snip_path>` when clicked.

use std::path::{Path, PathBuf};
use std::process::Command;

use ashpd::desktop::notification::{Button, Notification, NotificationProxy};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Sends a desktop notification for a copied snip and listens for the `"edit"` action
/// to spawn `lx-snippingtool --editor <snip_path>`.
pub async fn notify_and_listen_for_edit(snip_path: PathBuf) {
    let notif_id = format!("lx-snip-{}", Uuid::new_v4());
    let proxy = match NotificationProxy::new().await {
        Ok(p) => p,
        Err(err) => {
            warn!(error = %err, "failed to create ashpd NotificationProxy");
            return;
        }
    };

    let notification = Notification::new("Snip copied to clipboard")
        .body("Click to open in the annotation editor.")
        .default_action("edit")
        .button(Button::new("Edit", "edit"));

    if let Err(err) = proxy.add_notification(&notif_id, notification).await {
        warn!(error = %err, "failed to send desktop notification");
        return;
    }

    let mut stream = match proxy.receive_action_invoked().await {
        Ok(s) => s,
        Err(err) => {
            debug!(error = %err, "failed to subscribe to notification action_invoked");
            return;
        }
    };

    use iced::futures::StreamExt;
    while let Some(action) = stream.next().await {
        if action.id() == notif_id && action.name() == "edit" {
            info!(path = %snip_path.display(), "notification clicked; launching editor");
            spawn_editor(&snip_path);
            let _ = proxy.remove_notification(&notif_id).await;
            break;
        }
    }
}

fn spawn_editor(snip_path: &Path) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(err) => {
            warn!(error = %err, "failed to resolve current_exe for editor launch");
            return;
        }
    };

    if let Err(err) = Command::new(exe).arg("--editor").arg(snip_path).spawn() {
        warn!(error = %err, path = %snip_path.display(), "failed to spawn --editor process");
    }
}
