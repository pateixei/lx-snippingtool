//! Módulo: `notification`
//! Objetivos: Enviar notificações desktop pós-cópia (`ashpd` no Linux; WinRT Toast Notification com `AppUserModelID` no Windows 11) e abrir o editor ao clicar em "Edit".
//! Principais itens: `notify_and_listen_for_edit` e `spawn_editor` (além de `APP_AUMID` e `ensure_aumid_registered` no Windows).
//! Última alteração: 2026-11-20

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{info, warn};

/// Identificador explícito `AppUserModelID` usado no registro de notificações Toast e janela no Windows 11.
#[cfg(target_os = "windows")]
pub const APP_AUMID: &str = "io.github.lx_snippingtool.SnippingTool";

/// Registra o `AppUserModelID` no registro do usuário e associa-o ao processo atual no Windows 11.
/// Input: nenhum | Output: nenhum (`()`).
#[cfg(target_os = "windows")]
pub fn ensure_aumid_registered() {
    use std::ffi::c_void;
    use std::sync::Once;
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    static AUMID_ONCE: Once = Once::new();
    AUMID_ONCE.call_once(|| {
        let subkey = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{APP_AUMID}"));
        let value_name = HSTRING::from("DisplayName");
        let display_wide: Vec<u16> = "LX Snipping Tool\0".encode_utf16().collect();
        let cb_bytes = (display_wide.len() * std::mem::size_of::<u16>()) as u32;
        unsafe {
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &subkey,
                &value_name,
                REG_SZ.0,
                Some(display_wide.as_ptr() as *const c_void),
                cb_bytes,
            );
            let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_AUMID));
        }
    });
}

/// Envia uma notificação de imagem copiada e aguarda o clique na ação "Edit" para abrir o editor.
/// Input: `snip_path` do PNG salvo localmente | Output: futuro assíncrono sem retorno (efeito colateral D-Bus no Linux).
#[cfg(target_os = "linux")]
pub async fn notify_and_listen_for_edit(snip_path: PathBuf) {
    use ashpd::desktop::notification::{Button, Notification, NotificationProxy};
    use tracing::debug;
    use uuid::Uuid;

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

/// Envia uma Toast Notification nativa do Windows 11 e escuta o clique na ação "Edit" por até 30 segundos.
/// Input: `snip_path` do PNG salvo localmente | Output: futuro assíncrono aguardando ativação, descarte ou timeout.
#[cfg(target_os = "windows")]
pub async fn notify_and_listen_for_edit(snip_path: PathBuf) {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::oneshot;
    use windows::core::{IInspectable, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{
        ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification, ToastNotificationManager,
    };

    use crate::capture_windows::WinRtScope;

    ensure_aumid_registered();
    let (tx, rx) = oneshot::channel::<bool>();
    let tx_slot = Arc::new(Mutex::new(Some(tx)));

    let send_res = tokio::task::spawn_blocking(move || -> Result<ToastNotification, String> {
        let _winrt = WinRtScope::enter_mta().map_err(|e| e.to_string())?;

        let xml_str = r#"<toast launch="action=edit"><visual><binding template="ToastGeneric"><text>Snip copied to clipboard</text><text>Click to open in the annotation editor.</text></binding></visual><actions><action content="Edit" arguments="action=edit" activationType="foreground"/></actions></toast>"#;
        let doc = XmlDocument::new().map_err(|e| format!("XmlDocument::new failed: {e}"))?;
        doc.LoadXml(&HSTRING::from(xml_str))
            .map_err(|e| format!("XmlDocument::LoadXml failed: {e}"))?;

        let toast = ToastNotification::CreateToastNotification(&doc)
            .map_err(|e| format!("CreateToastNotification failed: {e}"))?;

        let tx_act = Arc::clone(&tx_slot);
        let path_act = snip_path.clone();
        toast
            .Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
                move |_, _| {
                    info!(path = %path_act.display(), "Windows toast notification activated; launching editor");
                    spawn_editor(&path_act);
                    if let Ok(mut guard) = tx_act.lock() {
                        if let Some(sender) = guard.take() {
                            let _ = sender.send(true);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(|e| format!("ToastNotification::Activated failed: {e}"))?;

        let tx_dis = Arc::clone(&tx_slot);
        toast
            .Dismissed(&TypedEventHandler::<ToastNotification, ToastDismissedEventArgs>::new(
                move |_, _| {
                    if let Ok(mut guard) = tx_dis.lock() {
                        if let Some(sender) = guard.take() {
                            let _ = sender.send(false);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(|e| format!("ToastNotification::Dismissed failed: {e}"))?;

        let tx_fail = Arc::clone(&tx_slot);
        toast
            .Failed(&TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(
                move |_, _| {
                    if let Ok(mut guard) = tx_fail.lock() {
                        if let Some(sender) = guard.take() {
                            let _ = sender.send(false);
                        }
                    }
                    Ok(())
                },
            ))
            .map_err(|e| format!("ToastNotification::Failed failed: {e}"))?;

        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_AUMID))
            .map_err(|e| format!("CreateToastNotifierWithId failed: {e}"))?;
        notifier
            .Show(&toast)
            .map_err(|e| format!("ToastNotifier::Show failed: {e}"))?;
        Ok(toast)
    })
    .await;

    match send_res {
        Ok(Ok(_keep_toast)) => {
            let _ = tokio::time::timeout(Duration::from_secs(30), rx).await;
        }
        Ok(Err(err)) => {
            warn!(error = %err, "failed to show Windows toast notification");
        }
        Err(err) => {
            warn!(error = %err, "Windows toast task panicked");
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
