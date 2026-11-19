//! Módulo: `notification`
//! Objetivos: Enviar notificações desktop pós-cópia via `ashpd` e abrir o editor ao clicar na ação "Edit".
//! Principais itens: `notify_and_listen_for_edit` e `spawn_editor`.
//! Última alteração: 2026-11-19

use std::path::{Path, PathBuf};
use std::process::Command;

use ashpd::desktop::notification::{Button, Notification, NotificationProxy};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Envia uma notificação de imagem copiada e aguarda o clique na ação "Edit" para abrir o editor.
/// Input: `snip_path` do PNG salvo localmente | Output: futuro assíncrono sem retorno (efeito colateral D-Bus).
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

/// Inicia uma nova instância do executável com `--editor <snip_path>` de forma destacada.
/// Input: `snip_path` do arquivo PNG | Output: nenhum (gera processo filho desacoplado).
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
