//! Módulo: `telemetry`
//! Objetivos: Inicializar logging estruturado (`tracing` no `stderr` e `journald`) e registrar hook global de panic.
//! Principais itens: `init_telemetry` e `install_panic_hook`.
//! Última alteração: 2026-11-19

use std::backtrace::Backtrace;
use std::sync::Once;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

static TELEMETRY_INIT: Once = Once::new();

/// Inicializa o sistema de logs (`stderr` + `journald` quando disponível) e o hook de panic uma única vez.
/// Input: nenhum | Output: nenhum (efeito colateral idempotente de inicialização global).
pub fn init_telemetry() {
    TELEMETRY_INIT.call_once(|| {
        let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(
                "info,wgpu_core=warn,wgpu_hal=warn,iced_wgpu=warn,naga=warn,sctk_adwaita=error",
            )
        });

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(true);

        // Omit journald cleanly when the journald socket is unavailable (e.g. containers/CI).
        let journald_layer = tracing_journald::layer().ok();

        let _ = tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(journald_layer)
            .try_init();

        install_panic_hook();
    });
}

/// Substitui o hook padrão de panic para registrar thread, localização e backtrace via `tracing::error`.
/// Input: nenhum | Output: nenhum (instala o panic hook global).
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let current = std::thread::current();
        let thread_name = current.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };
        let backtrace = Backtrace::force_capture();

        tracing::error!(
            thread = thread_name,
            location = %location,
            payload = %payload,
            backtrace = %backtrace,
            "application panic"
        );

        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_telemetry_is_idempotent() {
        init_telemetry();
        init_telemetry();
    }
}
