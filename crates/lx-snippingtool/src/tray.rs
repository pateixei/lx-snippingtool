//! Módulo: `tray`
//! Objetivos: Integrar o ícone opcional na bandeja (`--tray` via `ksni` no Linux ou `Shell_NotifyIconW` + atalho global `RegisterHotKey` no Windows 11) e fornecer o indicador ao vivo de gravação (`StatusNotifierItem` D-Bus no Linux ou bandeja Win32 no Windows).
//! Principais itens: `SnippingTray` (Linux), `spawn_tray_if_available`, `run_resident_tray_and_hotkey` (Windows), `TrayRecordingCmd`, `RecordingTrayHandle`, `spawn_recording_tray`, `format_top_bar_label` e `render_tray_icon`.
//! Última alteração: 2026-11-20

use std::process::Command;

use tracing::warn;

/// Representa comandos enviados pelo menu da barra superior/bandeja durante uma gravação ativa.
/// Armazena: variantes `TogglePause`, `Stop` e `ToggleFloatingBar` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayRecordingCmd {
    /// Alterna entre pausar e retomar a gravação atual.
    TogglePause,
    /// Encerra e finaliza a gravação em andamento.
    Stop,
    /// Exibe ou oculta a janela auxiliar flutuante de gravação.
    ToggleFloatingBar,
}

/// Formata o rótulo textual ao vivo exibido ao lado do ícone ou tooltip (`REC MM:SS` / `PAUSADO MM:SS`).
/// Input: tempo `elapsed_secs` e flag `paused` | Output: `String` formatada.
pub fn format_top_bar_label(elapsed_secs: u64, paused: bool) -> String {
    let mins = elapsed_secs / 60;
    let secs = elapsed_secs % 60;
    if paused {
        format!("PAUSADO {mins:02}:{secs:02}")
    } else {
        format!("REC {mins:02}:{secs:02}")
    }
}

/// Executa o próprio binário atual em um subprocesso destacado com os argumentos informados.
/// Input: slice de argumentos `args` | Output: nenhum (efeito colateral de spawn).
fn spawn_self(args: &[&str]) {
    let Ok(exe) = std::env::current_exe() else {
        warn!("failed to resolve current_exe for tray action");
        return;
    };
    if let Err(err) = Command::new(exe).args(args).spawn() {
        warn!(error = %err, ?args, "failed to spawn lx-snippingtool from tray");
    }
}

/// Renderiza um ícone 24x24 em ARGB32 (vermelho pulsante gravando ou âmbar com barras em pausa).
/// Input: flags `paused` e `pulse_bright` | Output: tupla `(width, height, bytes_argb32)`.
pub fn render_tray_icon(paused: bool, pulse_bright: bool) -> (i32, i32, Vec<u8>) {
    const SIZE: usize = 24;
    let mut data = vec![0u8; SIZE * SIZE * 4];
    let center = (SIZE as f32 - 1.0) * 0.5;
    let outer_r = 11.2f32;
    let inner_r = 8.8f32;

    let (fill_r, fill_g, fill_b) = if paused {
        (245u8, 158u8, 11u8)
    } else if pulse_bright {
        (255u8, 30u8, 30u8)
    } else {
        (185u8, 28u8, 28u8)
    };

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let offset = (y * SIZE + x) * 4;

            if dist <= inner_r {
                if paused {
                    let is_pause_bar =
                        (7..=16).contains(&y) && ((8..=10).contains(&x) || (13..=15).contains(&x));
                    if is_pause_bar {
                        data[offset] = 255;
                        data[offset + 1] = 255;
                        data[offset + 2] = 255;
                        data[offset + 3] = 255;
                    } else {
                        data[offset] = 255;
                        data[offset + 1] = fill_r;
                        data[offset + 2] = fill_g;
                        data[offset + 3] = fill_b;
                    }
                } else if pulse_bright && dist <= 3.6 {
                    // Glowing inner highlight dot on bright pulse frames.
                    data[offset] = 255;
                    data[offset + 1] = 255;
                    data[offset + 2] = 228;
                    data[offset + 3] = 230;
                } else {
                    data[offset] = 255;
                    data[offset + 1] = fill_r;
                    data[offset + 2] = fill_g;
                    data[offset + 3] = fill_b;
                }
            } else if dist <= outer_r {
                let alpha = ((outer_r - dist).clamp(0.0, 1.0) * 255.0).round() as u8;
                data[offset] = alpha;
                data[offset + 1] = 255;
                data[offset + 2] = 255;
                data[offset + 3] = 255;
            }
        }
    }

    (SIZE as i32, SIZE as i32, data)
}

#[cfg(target_os = "linux")]
mod platform_tray {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
    use tracing::debug;
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::{ObjectPath, OwnedValue, StructureBuilder, Value};
    use zbus::Connection;

    use super::{format_top_bar_label, render_tray_icon, spawn_self, TrayRecordingCmd};

    static REC_TRAY_SEQ: AtomicUsize = AtomicUsize::new(1);

    const SNI_PATH: &str = "/StatusNotifierItem";
    const MENU_PATH: &str = "/MenuBar";

    /// Define o item opcional de bandeja do aplicativo acionado com `--tray`.
    /// Armazena: estrutura unitária sem campos | Principais métodos: `id`, `title`, `icon_name`, `activate`, `menu`.
    #[derive(Debug, Default)]
    pub struct SnippingTray;

    impl ksni::Tray for SnippingTray {
        /// Retorna o identificador único do item na especificação StatusNotifierItem.
        /// Input: `&self` | Output: `String` com o App ID.
        fn id(&self) -> String {
            "io.github.lx_snippingtool.SnippingTool".to_string()
        }

        /// Retorna o título amigável exibido pelo ambiente de área de trabalho.
        /// Input: `&self` | Output: `String` com o título do app.
        fn title(&self) -> String {
            "LX Snipping Tool".to_string()
        }

        /// Informa o nome do ícone de tema Freedesktop utilizado pelo tray.
        /// Input: `&self` | Output: `String` `"applets-screenshooter"`.
        fn icon_name(&self) -> String {
            "applets-screenshooter".to_string()
        }

        /// Trata o clique principal no ícone da bandeja disparando uma nova captura retangular.
        /// Input: `&mut self` e coordenadas `_x`/`_y` | Output: nenhum (inicia novo processo).
        fn activate(&mut self, _x: i32, _y: i32) {
            spawn_self(&["--mode", "rect"]);
        }

        /// Constrói o menu de contexto com atalhos para captura, gravação, histórico e saída.
        /// Input: `&self` | Output: vetor `Vec<ksni::MenuItem<Self>>`.
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            vec![
                StandardItem {
                    label: "New Snip (Rect)".to_string(),
                    activate: Box::new(|_| spawn_self(&["--mode", "rect"])),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Record Screen".to_string(),
                    activate: Box::new(|_| spawn_self(&["--mode", "record"])),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: "Snip History".to_string(),
                    activate: Box::new(|_| spawn_self(&["--history"])),
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
                StandardItem {
                    label: "Quit".to_string(),
                    activate: Box::new(|_| std::process::exit(0)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// Registra o serviço `ksni` caso o ambiente possua um `StatusNotifierWatcher` ativo.
    /// Input: nenhum | Output: nenhum (falhas são apenas registradas em `debug`).
    pub fn spawn_tray_if_available() {
        use ksni::blocking::TrayMethods;
        match SnippingTray.spawn() {
            Ok(_handle) => {
                debug!("ksni StatusNotifierItem tray service spawned");
            }
            Err(err) => {
                debug!(error = %err, "ksni StatusNotifierItem tray unavailable; continuing without tray");
            }
        }
    }

    /// Guarda o estado compartilhado síncrono entre as interfaces D-Bus do indicador e a UI.
    /// Armazena: `elapsed_secs`, `paused`, `floating_bar_visible`, `revision` e canal `tx` | Principais métodos: `formatted_elapsed`, `label_text`, `pulse_bright`, `item_props`.
    #[derive(Debug)]
    pub(super) struct TrayState {
        pub(super) elapsed_secs: u64,
        pub(super) paused: bool,
        pub(super) floating_bar_visible: bool,
        pub(super) revision: u32,
        pub(super) tx: UnboundedSender<TrayRecordingCmd>,
    }

    impl TrayState {
        /// Formata a duração decorrida em minutos e segundos (`MM:SS`).
        /// Input: `&self` | Output: `String` formatada.
        fn formatted_elapsed(&self) -> String {
            let mins = self.elapsed_secs / 60;
            let secs = self.elapsed_secs % 60;
            format!("{mins:02}:{secs:02}")
        }

        /// Gera o texto curto de status usado em título, tooltip e `XAyatanaLabel`.
        /// Input: `&self` | Output: `String` de status ao vivo.
        fn label_text(&self) -> String {
            format_top_bar_label(self.elapsed_secs, self.paused)
        }

        /// Alterna a luminosidade do ícone segundo a segundo apenas quando a gravação está ativa.
        /// Input: `&self` | Output: `true` nos segundos pares não pausados.
        fn pulse_bright(&self) -> bool {
            !self.paused && self.elapsed_secs.is_multiple_of(2)
        }

        /// Monta o dicionário de propriedades `com.canonical.dbusmenu` para o item solicitado.
        /// Input: `&self`, `id` do item e filtro `property_names` | Output: `Option<HashMap<String, OwnedValue>>`.
        fn item_props(
            &self,
            id: i32,
            property_names: &[String],
        ) -> Option<HashMap<String, OwnedValue>> {
            let mut map = HashMap::new();
            let want =
                |name: &str| property_names.is_empty() || property_names.iter().any(|p| p == name);

            match id {
                0 => {
                    if want("children-display") {
                        map.insert("children-display".to_string(), str_val("submenu"));
                    }
                }
                1 => {
                    let ts = self.formatted_elapsed();
                    let label = if self.paused {
                        format!("⏸ Pausado — {ts}")
                    } else {
                        format!("● Gravando — {ts}")
                    };
                    if want("label") {
                        map.insert("label".to_string(), str_val(&label));
                    }
                    if want("enabled") {
                        map.insert("enabled".to_string(), bool_val(false));
                    }
                }
                2 | 5 => {
                    if want("type") {
                        map.insert("type".to_string(), str_val("separator"));
                    }
                }
                3 => {
                    let label = if self.paused {
                        "Retomar gravação (clique do meio)"
                    } else {
                        "Pausar gravação (clique do meio)"
                    };
                    if want("label") {
                        map.insert("label".to_string(), str_val(label));
                    }
                }
                4 => {
                    if want("label") {
                        map.insert(
                            "label".to_string(),
                            str_val("Parar gravação (duplo clique)"),
                        );
                    }
                }
                6 => {
                    let label = if self.floating_bar_visible {
                        "Ocultar barra flutuante"
                    } else {
                        "Mostrar barra flutuante"
                    };
                    if want("label") {
                        map.insert("label".to_string(), str_val(label));
                    }
                }
                _ => return None,
            }
            Some(map)
        }
    }

    /// Converte uma string em `OwnedValue` D-Bus para propriedades do menu.
    /// Input: slice `&str` `s` | Output: `OwnedValue` contendo a string.
    fn str_val(s: &str) -> OwnedValue {
        Value::from(s)
            .try_into_owned()
            .expect("string converts to OwnedValue")
    }

    /// Converte um booleano em `OwnedValue` D-Bus para propriedades do menu.
    /// Input: booleano `b` | Output: `OwnedValue` contendo o booleano.
    fn bool_val(b: bool) -> OwnedValue {
        Value::from(b)
            .try_into_owned()
            .expect("bool converts to OwnedValue")
    }

    type SniPixmap = (i32, i32, Vec<u8>);
    type SniToolTip = (String, Vec<SniPixmap>, String, String);
    type MenuLayout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

    /// Empacota uma tupla de layout do `dbusmenu` em um `OwnedValue` variante D-Bus.
    /// Input: tupla `layout` | Output: `OwnedValue` com estrutura `(ia{sv}av)`.
    fn layout_to_owned_value(layout: MenuLayout) -> OwnedValue {
        let s = StructureBuilder::new()
            .add_field(layout.0)
            .add_field(layout.1)
            .add_field(layout.2)
            .build()
            .expect("valid dbusmenu layout structure");
        Value::from(s)
            .try_into_owned()
            .expect("layout converts to OwnedValue")
    }

    /// Cliente D-Bus do serviço observador `org.kde.StatusNotifierWatcher` da sessão.
    /// Armazena: proxy `zbus` gerado | Principais métodos: `register_status_notifier_item`, `is_status_notifier_host_registered`.
    #[zbus::proxy(
        interface = "org.kde.StatusNotifierWatcher",
        default_service = "org.kde.StatusNotifierWatcher",
        default_path = "/StatusNotifierWatcher"
    )]
    trait StatusNotifierWatcher {
        /// Registra o serviço StatusNotifierItem deste processo no painel do ambiente.
        /// Input: nome D-Bus `service` | Output: `zbus::Result<()>`.
        async fn register_status_notifier_item(&self, service: &str) -> zbus::Result<()>;

        /// Indica se o painel possui ao menos um host StatusNotifier registrado.
        /// Input: `&self` | Output: `zbus::Result<bool>`.
        #[zbus(property)]
        fn is_status_notifier_host_registered(&self) -> zbus::Result<bool>;
    }

    /// Expõe a interface D-Bus `org.kde.StatusNotifierItem` para o indicador de gravação na barra superior.
    /// Armazena: referência compartilhada `state` | Principais métodos: propriedades SNI, `x_ayatana_label` e sinais de atualização.
    pub(super) struct RecordingSni {
        pub(super) state: Arc<Mutex<TrayState>>,
    }

    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl RecordingSni {
        /// Informa que o menu de contexto próprio não é usado pois o host lê `/MenuBar`.
        /// Input: `&self` e coordenadas `_x`/`_y` | Output: erro `UnknownMethod`.
        async fn context_menu(&self, _x: i32, _y: i32) -> zbus::fdo::Result<()> {
            Err(zbus::fdo::Error::UnknownMethod(
                "Not supported, use Menu".into(),
            ))
        }

        /// Encerra a gravação diretamente em duplo clique esquerdo no indicador da barra sem abrir o menu.
        /// Input: `&self` e coordenadas `_x`/`_y` | Output: `Ok(())`.
        pub(super) async fn activate(&self, _x: i32, _y: i32) -> zbus::fdo::Result<()> {
            let g = self.state.lock().expect("tray mutex poisoned");
            let _ = g.tx.send(TrayRecordingCmd::Stop);
            Ok(())
        }

        /// Alterna pausa/retomada via ativação secundária padrão (`SecondaryActivate`, clique do meio) sem abrir menu.
        /// Input: `&self` e coordenadas `_x`/`_y` | Output: `Ok(())`.
        pub(super) async fn secondary_activate(&self, _x: i32, _y: i32) -> zbus::fdo::Result<()> {
            let g = self.state.lock().expect("tray mutex poisoned");
            let _ = g.tx.send(TrayRecordingCmd::TogglePause);
            Ok(())
        }

        /// Alterna pausa/retomada via método `XAyatanaSecondaryActivate` usado nativamente pelo `ubuntu-appindicators` no clique do meio.
        /// Input: `&self` e `_timestamp` (`u32`) | Output: `Ok(())`.
        #[zbus(name = "XAyatanaSecondaryActivate")]
        pub(super) async fn x_ayatana_secondary_activate(
            &self,
            _timestamp: u32,
        ) -> zbus::fdo::Result<()> {
            let g = self.state.lock().expect("tray mutex poisoned");
            let _ = g.tx.send(TrayRecordingCmd::TogglePause);
            Ok(())
        }

        /// Ignora eventos de rolagem do mouse sobre o indicador.
        /// Input: `&self`, `_delta` e direção `_dir` | Output: `Ok(())`.
        async fn scroll(&self, _delta: i32, _dir: String) -> zbus::fdo::Result<()> {
            Ok(())
        }

        /// Aceita o token de ativação XDG enviado pelo compositor sem necessidade de uso.
        /// Input: `&self` e `_token` | Output: `Ok(())`.
        async fn provide_xdg_activation_token(&self, _token: String) -> zbus::fdo::Result<()> {
            Ok(())
        }

        /// Categoria SNI do indicador (`ApplicationStatus`).
        /// Input: `&self` | Output: `&'static str`.
        #[zbus(property)]
        fn category(&self) -> &'static str {
            "ApplicationStatus"
        }

        /// Identificador exclusivo do indicador de gravação no D-Bus.
        /// Input: `&self` | Output: `&'static str`.
        #[zbus(property)]
        fn id(&self) -> &'static str {
            "io.github.lx_snippingtool.Recorder"
        }

        /// Título dinâmico contendo o estado atual e o cronômetro (`REC MM:SS` ou `PAUSADO MM:SS`).
        /// Input: `&self` | Output: `String` com o texto de status.
        #[zbus(property)]
        fn title(&self) -> String {
            let g = self.state.lock().expect("tray mutex poisoned");
            g.label_text()
        }

        /// Mantém o status sempre como `"Active"` para visibilidade contínua no painel.
        /// Input: `&self` | Output: `&'static str` `"Active"`.
        #[zbus(property)]
        fn status(&self) -> &'static str {
            "Active"
        }

        /// Identificador de janela associado ao indicador (não aplicável no Wayland).
        /// Input: `&self` | Output: `0i32`.
        #[zbus(property)]
        fn window_id(&self) -> i32 {
            0
        }

        /// Caminho adicional de tema de ícones (vazio pois usamos `IconPixmap` em memória).
        /// Input: `&self` | Output: `&'static str` `""`.
        #[zbus(property)]
        fn icon_theme_path(&self) -> &'static str {
            ""
        }

        /// Caminho do objeto D-Bus que exporta o menu `com.canonical.dbusmenu`.
        /// Input: `&self` | Output: `ObjectPath` `/MenuBar`.
        #[zbus(property)]
        fn menu(&self) -> ObjectPath<'static> {
            ObjectPath::from_static_str_unchecked(MENU_PATH)
        }

        /// Indica ao shell que o indicador abre seu menu ao primeiro clique.
        /// Input: `&self` | Output: `true`.
        #[zbus(property)]
        fn item_is_menu(&self) -> bool {
            true
        }

        /// Nome de ícone de tema vazio para priorizar o badge pulsante em `IconPixmap`.
        /// Input: `&self` | Output: `&'static str` `""`.
        #[zbus(property)]
        fn icon_name(&self) -> &'static str {
            ""
        }

        /// Retorna o badge 24x24 ARGB32 pulsante (vermelho gravando ou âmbar pausado).
        /// Input: `&self` | Output: `Vec<SniPixmap>`.
        #[zbus(property)]
        fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            let g = self.state.lock().expect("tray mutex poisoned");
            vec![render_tray_icon(g.paused, g.pulse_bright())]
        }

        /// Nome do ícone de sobreposição (não utilizado).
        /// Input: `&self` | Output: `&'static str` `""`.
        #[zbus(property)]
        fn overlay_icon_name(&self) -> &'static str {
            ""
        }

        /// Lista de pixmaps de sobreposição (vazia).
        /// Input: `&self` | Output: vetor vazio.
        #[zbus(property)]
        fn overlay_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            Vec::new()
        }

        /// Nome do ícone de atenção (não utilizado).
        /// Input: `&self` | Output: `&'static str` `""`.
        #[zbus(property)]
        fn attention_icon_name(&self) -> &'static str {
            ""
        }

        /// Lista de pixmaps de atenção (vazia).
        /// Input: `&self` | Output: vetor vazio.
        #[zbus(property)]
        fn attention_icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
            Vec::new()
        }

        /// Nome de animação de atenção (não utilizado).
        /// Input: `&self` | Output: `&'static str` `""`.
        #[zbus(property)]
        fn attention_movie_name(&self) -> &'static str {
            ""
        }

        /// Tooltip exibido ao passar o cursor sobre o ícone da barra superior.
        /// Input: `&self` | Output: tupla `SniToolTip`.
        #[zbus(property)]
        fn tool_tip(&self) -> SniToolTip {
            let g = self.state.lock().expect("tray mutex poisoned");
            (
                String::new(),
                Vec::new(),
                g.label_text(),
                "Clique do meio: Pausar/Retomar • Duplo clique: Parar • Clique: Menu".to_string(),
            )
        }

        /// Texto ao vivo exibido diretamente na barra do Ubuntu AppIndicator (`REC MM:SS`).
        /// Input: `&self` | Output: `String` com cronômetro atual.
        #[zbus(property, name = "XAyatanaLabel")]
        fn x_ayatana_label(&self) -> String {
            let g = self.state.lock().expect("tray mutex poisoned");
            g.label_text()
        }

        /// Texto guia usado pelo painel para reservar largura estável para o cronômetro.
        /// Input: `&self` | Output: `&'static str` `"PAUSADO 00:00"`.
        #[zbus(property, name = "XAyatanaLabelGuide")]
        fn x_ayatana_label_guide(&self) -> &'static str {
            "PAUSADO 00:00"
        }

        /// Sinal D-Bus notificando mudança de título do indicador.
        /// Input: emissor `ctxt` | Output: `zbus::Result<()>`.
        #[zbus(signal)]
        async fn new_title(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

        /// Sinal D-Bus notificando novo pixmap do indicador.
        /// Input: emissor `ctxt` | Output: `zbus::Result<()>`.
        #[zbus(signal)]
        async fn new_icon(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

        /// Sinal D-Bus notificando atualização de tooltip.
        /// Input: emissor `ctxt` | Output: `zbus::Result<()>`.
        #[zbus(signal)]
        async fn new_tool_tip(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

        /// Sinal D-Bus `XAyatanaNewLabel` disparando redesenho instantâneo do cronômetro no painel.
        /// Input: emissor `ctxt`, `label` e `guide` | Output: `zbus::Result<()>`.
        #[zbus(signal, name = "XAyatanaNewLabel")]
        async fn x_ayatana_new_label(
            ctxt: &SignalEmitter<'_>,
            label: &str,
            guide: &str,
        ) -> zbus::Result<()>;
    }

    /// Expõe a interface `com.canonical.dbusmenu` com os comandos de Pausar, Parar e Barra Flutuante.
    /// Armazena: referência compartilhada `state` | Principais métodos: `get_layout`, `get_group_properties`, `event`.
    struct RecordingDbusMenu {
        state: Arc<Mutex<TrayState>>,
    }

    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl RecordingDbusMenu {
        /// Retorna a árvore de itens e propriedades do menu a partir de `parent_id`.
        /// Input: `&self`, `parent_id`, `recursion_depth` e `property_names` | Output: `(revision, MenuLayout)`.
        async fn get_layout(
            &self,
            parent_id: i32,
            recursion_depth: i32,
            property_names: Vec<String>,
        ) -> zbus::fdo::Result<(u32, MenuLayout)> {
            let g = self.state.lock().expect("tray mutex poisoned");
            let rev = g.revision;
            let props = g
                .item_props(parent_id, &property_names)
                .ok_or_else(|| zbus::fdo::Error::InvalidArgs("parentId not found".into()))?;

            let children = if parent_id == 0 && recursion_depth != 0 {
                (1..=6)
                    .filter_map(|cid| {
                        g.item_props(cid, &property_names)
                            .map(|cp| layout_to_owned_value((cid, cp, Vec::new())))
                    })
                    .collect()
            } else {
                Vec::new()
            };

            Ok((rev, (parent_id, props, children)))
        }

        /// Consulta propriedades em lote para os identificadores informados do menu.
        /// Input: `&self`, lista `ids` e filtro `property_names` | Output: vetor de `(id, mapa)`.
        async fn get_group_properties(
            &self,
            ids: Vec<i32>,
            property_names: Vec<String>,
        ) -> zbus::fdo::Result<Vec<(i32, HashMap<String, OwnedValue>)>> {
            let g = self.state.lock().expect("tray mutex poisoned");
            let target_ids: Vec<i32> = if ids.is_empty() {
                (0..=6).collect()
            } else {
                ids
            };
            Ok(target_ids
                .into_iter()
                .filter_map(|id| g.item_props(id, &property_names).map(|p| (id, p)))
                .collect())
        }

        /// Consulta o valor de uma propriedade específica em um item de menu.
        /// Input: `&self`, `id` do item e `name` | Output: `zbus::fdo::Result<OwnedValue>`.
        async fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
            let g = self.state.lock().expect("tray mutex poisoned");
            let props = g
                .item_props(id, std::slice::from_ref(&name))
                .ok_or_else(|| zbus::fdo::Error::InvalidArgs("id not found".into()))?;
            props
                .into_iter()
                .next()
                .map(|(_, v)| v)
                .ok_or_else(|| zbus::fdo::Error::InvalidArgs("property not found".into()))
        }

        /// Processa cliques e eventos em um item do menu enviando o comando correspondente ao app.
        /// Input: `&self`, `id`, `event_id`, `_data` e `_timestamp` | Output: `zbus::fdo::Result<()>`.
        async fn event(
            &self,
            id: i32,
            event_id: String,
            _data: OwnedValue,
            _timestamp: u32,
        ) -> zbus::fdo::Result<()> {
            if !(0..=6).contains(&id) {
                return Err(zbus::fdo::Error::InvalidArgs("id not found".into()));
            }
            if event_id == "clicked" {
                let g = self.state.lock().expect("tray mutex poisoned");
                match id {
                    3 => {
                        let _ = g.tx.send(TrayRecordingCmd::TogglePause);
                    }
                    4 => {
                        let _ = g.tx.send(TrayRecordingCmd::Stop);
                    }
                    6 => {
                        let _ = g.tx.send(TrayRecordingCmd::ToggleFloatingBar);
                    }
                    _ => {}
                }
            }
            Ok(())
        }

        /// Processa múltiplos eventos do menu em lote retornando eventuais IDs inexistentes.
        /// Input: `&self` e lista `events` | Output: `zbus::fdo::Result<Vec<i32>>`.
        async fn event_group(
            &self,
            events: Vec<(i32, String, OwnedValue, u32)>,
        ) -> zbus::fdo::Result<Vec<i32>> {
            if events.is_empty() {
                return Err(zbus::fdo::Error::InvalidArgs("Empty events".into()));
            }
            let mut not_found = Vec::new();
            for (id, event_id, _data, _ts) in &events {
                if !(0..=6).contains(id) {
                    not_found.push(*id);
                    continue;
                }
                if event_id == "clicked" {
                    let g = self.state.lock().expect("tray mutex poisoned");
                    match id {
                        3 => {
                            let _ = g.tx.send(TrayRecordingCmd::TogglePause);
                        }
                        4 => {
                            let _ = g.tx.send(TrayRecordingCmd::Stop);
                        }
                        6 => {
                            let _ = g.tx.send(TrayRecordingCmd::ToggleFloatingBar);
                        }
                        _ => {}
                    }
                }
            }
            if not_found.len() == events.len() {
                Err(zbus::fdo::Error::InvalidArgs("No matching id".into()))
            } else {
                Ok(not_found)
            }
        }

        /// Responde ao aviso de pré-exibição de submenu sem exigir reconstrução assíncrona.
        /// Input: `&self` e `id` | Output: `zbus::fdo::Result<bool>`.
        async fn about_to_show(&self, id: i32) -> zbus::fdo::Result<bool> {
            if !(0..=6).contains(&id) {
                return Err(zbus::fdo::Error::InvalidArgs("id not found".into()));
            }
            Ok(false)
        }

        /// Trata o aviso de pré-exibição em lote para múltiplos IDs de menu.
        /// Input: `&self` e lista `ids` | Output: tupla com vetores de atualização/não encontrados.
        async fn about_to_show_group(
            &self,
            ids: Vec<i32>,
        ) -> zbus::fdo::Result<(Vec<i32>, Vec<i32>)> {
            let not_found: Vec<i32> = ids.into_iter().filter(|id| !(0..=6).contains(id)).collect();
            Ok((Vec::new(), not_found))
        }

        /// Informa a versão do protocolo `com.canonical.dbusmenu` implementada.
        /// Input: `&self` | Output: versão `3u32`.
        #[zbus(property)]
        fn version(&self) -> u32 {
            3
        }

        /// Direção textual padrão usada pelos itens de menu.
        /// Input: `&self` | Output: `&'static str` `"ltr"`.
        #[zbus(property)]
        fn text_direction(&self) -> &'static str {
            "ltr"
        }

        /// Estado geral de exibição do menu D-Bus.
        /// Input: `&self` | Output: `&'static str` `"normal"`.
        #[zbus(property)]
        fn status(&self) -> &'static str {
            "normal"
        }

        /// Sinal emitido sempre que os rótulos ou estados dos itens do menu são alterados.
        /// Input: emissor `ctxt`, `revision` e `parent` | Output: `zbus::Result<()>`.
        #[zbus(signal)]
        async fn layout_updated(
            ctxt: &SignalEmitter<'_>,
            revision: u32,
            parent: i32,
        ) -> zbus::Result<()>;
    }

    /// Mantém a conexão D-Bus ativa e atualiza o indicador de gravação na barra superior.
    /// Armazena: `conn` e `state` compartilhado | Principais métodos: `update`, `shutdown`.
    #[derive(Clone)]
    pub struct RecordingTrayHandle {
        conn: Connection,
        state: Arc<Mutex<TrayState>>,
    }

    impl RecordingTrayHandle {
        /// Atualiza cronômetro, ícone pulsante, rótulo `XAyatanaLabel` e itens do menu na barra superior.
        /// Input: `&self`, `elapsed_secs`, `paused` e `floating_bar_visible` | Output: futuro sem retorno.
        pub async fn update(&self, elapsed_secs: u64, paused: bool, floating_bar_visible: bool) {
            let (label, rev) = {
                let mut g = self.state.lock().expect("tray mutex poisoned");
                g.elapsed_secs = elapsed_secs;
                g.paused = paused;
                g.floating_bar_visible = floating_bar_visible;
                g.revision = g.revision.saturating_add(1);
                (g.label_text(), g.revision)
            };

            let server = self.conn.object_server();
            if let Ok(sni_iface) = server.interface::<_, RecordingSni>(SNI_PATH).await {
                let emitter = sni_iface.signal_emitter();
                let sni = sni_iface.get().await;
                let _ = sni.x_ayatana_label_changed(emitter).await;
                let _ = RecordingSni::x_ayatana_new_label(emitter, &label, "PAUSADO 00:00").await;
                let _ = RecordingSni::new_icon(emitter).await;
                let _ = RecordingSni::new_title(emitter).await;
                let _ = RecordingSni::new_tool_tip(emitter).await;
            }
            if let Ok(menu_iface) = server.interface::<_, RecordingDbusMenu>(MENU_PATH).await {
                let _ =
                    RecordingDbusMenu::layout_updated(menu_iface.signal_emitter(), rev, 0).await;
            }
        }

        /// Fecha a conexão D-Bus removendo imediatamente o indicador da barra superior.
        /// Input: `&self` | Output: futuro concluído após fechar a conexão.
        pub async fn shutdown(&self) {
            let _ = self.conn.clone().close().await;
        }
    }

    /// Publica o serviço D-Bus de gravação e registra-o no painel do sistema operacional.
    /// Input: nenhum | Output: `Result<(RecordingTrayHandle, UnboundedReceiver<TrayRecordingCmd>), String>`.
    pub async fn spawn_recording_tray(
    ) -> Result<(RecordingTrayHandle, UnboundedReceiver<TrayRecordingCmd>), String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(TrayState {
            elapsed_secs: 0,
            paused: false,
            floating_bar_visible: false,
            revision: 1,
            tx,
        }));

        let well_known = format!(
            "org.kde.StatusNotifierItem-{}-{}",
            std::process::id(),
            REC_TRAY_SEQ.fetch_add(1, Ordering::AcqRel)
        );

        let conn = zbus::connection::Builder::session()
            .map_err(|e| format!("D-Bus session builder failed: {e}"))?
            .name(well_known.as_str())
            .map_err(|e| format!("D-Bus name request failed: {e}"))?
            .serve_at(
                SNI_PATH,
                RecordingSni {
                    state: Arc::clone(&state),
                },
            )
            .map_err(|e| format!("failed to serve StatusNotifierItem: {e}"))?
            .serve_at(
                MENU_PATH,
                RecordingDbusMenu {
                    state: Arc::clone(&state),
                },
            )
            .map_err(|e| format!("failed to serve DBusMenu: {e}"))?
            .build()
            .await
            .map_err(|e| format!("D-Bus session connection failed: {e}"))?;

        let watcher = StatusNotifierWatcherProxy::builder(&conn)
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|e| format!("StatusNotifierWatcher proxy failed: {e}"))?;

        if let Err(err) = watcher.register_status_notifier_item(&well_known).await {
            let _ = conn.close().await;
            return Err(format!("StatusNotifierWatcher registration failed: {err}"));
        }

        if !watcher
            .is_status_notifier_host_registered()
            .await
            .unwrap_or(false)
        {
            let _ = conn.close().await;
            return Err("No StatusNotifierHost registered on desktop panel".to_string());
        }

        Ok((RecordingTrayHandle { conn, state }, rx))
    }
}

#[cfg(target_os = "windows")]
mod platform_tray {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
    use tracing::warn;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
        ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
    };
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
        DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
        GetWindowLongPtrW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
        SetWindowLongPtrW, TrackPopupMenu, TranslateMessage, GWLP_USERDATA, HICON, ICONINFO,
        MF_SEPARATOR, MF_STRING, MSG, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_CLOSE,
        WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_MBUTTONUP, WM_RBUTTONUP, WM_USER,
        WNDCLASSW, WS_OVERLAPPED,
    };

    use super::{format_top_bar_label, render_tray_icon, spawn_self, TrayRecordingCmd};

    const HOTKEY_ID: i32 = 1;
    const WM_TRAY_CALLBACK: u32 = WM_USER + 1;
    const WM_REC_REFRESH: u32 = WM_USER + 2;

    const CMD_NEW_SNIP: usize = 1001;
    const CMD_RECORD: usize = 1002;
    const CMD_HISTORY: usize = 1003;
    const CMD_QUIT: usize = 1004;

    const CMD_REC_PAUSE: usize = 2001;
    const CMD_REC_STOP: usize = 2002;
    const CMD_REC_TOGGLE_BAR: usize = 2003;

    /// Converte um pixmap ARGB32 big-endian em `HICON` Win32 para uso em `NOTIFYICONDATAW`.
    /// Input: `width`, `height` e bytes `argb` | Output: `Option<HICON>`.
    fn create_hicon_from_argb(width: i32, height: i32, argb: &[u8]) -> Option<HICON> {
        if width <= 0 || height <= 0 || argb.len() < (width as usize) * (height as usize) * 4 {
            return None;
        }
        unsafe {
            let screen_dc = GetDC(HWND::default());
            let mem_dc = CreateCompatibleDC(screen_dc);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            let mut bits_ptr: *mut c_void = std::ptr::null_mut();
            let Ok(color_bmp): Result<HBITMAP, _> =
                CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
            else {
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(HWND::default(), screen_dc);
                return None;
            };

            if !bits_ptr.is_null() {
                let total = (width as usize) * (height as usize);
                let dst = std::slice::from_raw_parts_mut(bits_ptr as *mut u8, total * 4);
                for (src_px, dst_px) in argb
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(dst.as_chunks_mut::<4>().0)
                {
                    dst_px[0] = src_px[3]; // B
                    dst_px[1] = src_px[2]; // G
                    dst_px[2] = src_px[1]; // R
                    dst_px[3] = src_px[0]; // A
                }
            }

            let mask_bmp = CreateBitmap(width, height, 1, 1, None);
            let info = ICONINFO {
                fIcon: BOOL(1),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask_bmp,
                hbmColor: color_bmp,
            };
            let hicon = CreateIconIndirect(&info).ok();
            let _ = DeleteObject(mask_bmp);
            let _ = DeleteObject(color_bmp);
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(HWND::default(), screen_dc);
            hicon
        }
    }

    /// Renderiza o badge estático azul 24x24 ARGB32 usado pelo ícone residente na bandeja do Windows.
    /// Input: nenhum | Output: `(24, 24, bytes_argb32)`.
    fn render_idle_badge_argb() -> (i32, i32, Vec<u8>) {
        const SIZE: usize = 24;
        let mut data = vec![0u8; SIZE * SIZE * 4];
        let center = (SIZE as f32 - 1.0) * 0.5;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let dx = x as f32 - center;
                let dy = y as f32 - center;
                let dist = (dx * dx + dy * dy).sqrt();
                let off = (y * SIZE + x) * 4;
                if dist <= 4.2 {
                    data[off] = 255;
                    data[off + 1] = 255;
                    data[off + 2] = 255;
                    data[off + 3] = 255;
                } else if dist <= 9.2 {
                    data[off] = 255;
                    data[off + 1] = 0;
                    data[off + 2] = 120;
                    data[off + 3] = 212;
                } else if dist <= 11.2 {
                    let a = ((11.2 - dist).clamp(0.0, 1.0) * 255.0).round() as u8;
                    data[off] = a;
                    data[off + 1] = 255;
                    data[off + 2] = 255;
                    data[off + 3] = 255;
                }
            }
        }
        (SIZE as i32, SIZE as i32, data)
    }

    /// Copia até 127 caracteres UTF-16 terminados em NUL para `NOTIFYICONDATAW::szTip`.
    /// Input: `dst` (`&mut [u16; 128]`) e `text` (`&str`) | Output: nenhum (`()`).
    fn write_tip(dst: &mut [u16; 128], text: &str) {
        dst.fill(0);
        for (i, ch) in text.encode_utf16().take(127).enumerate() {
            dst[i] = ch;
        }
    }

    /// Exibe o menu de contexto do ícone residente da bandeja (`New Snip`, `Record Screen`, `Snip History`, `Quit`).
    /// Input: `hwnd` (`HWND`) | Output: nenhum (`()`).
    unsafe fn show_resident_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            CMD_NEW_SNIP,
            &HSTRING::from("New Snip (Ctrl+Shift+S)"),
        );
        let _ = AppendMenuW(menu, MF_STRING, CMD_RECORD, &HSTRING::from("Record Screen"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_HISTORY, &HSTRING::from("Snip History"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, &HSTRING::from("Quit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 as usize {
            CMD_NEW_SNIP => spawn_self(&["--mode", "rect"]),
            CMD_RECORD => spawn_self(&["--mode", "record"]),
            CMD_HISTORY => spawn_self(&["--history"]),
            CMD_QUIT => PostQuitMessage(0),
            _ => {}
        }
    }

    /// Procedure Win32 da janela oculta residente que atende `Ctrl+Shift+S` (`WM_HOTKEY`) e cliques na bandeja.
    /// Input: parâmetros padrão `WNDPROC` Win32 | Output: `LRESULT`.
    unsafe extern "system" fn resident_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
                spawn_self(&["--mode", "rect"]);
                LRESULT(0)
            }
            WM_TRAY_CALLBACK => {
                let mouse_msg = (lparam.0 & 0xFFFF) as u32;
                if mouse_msg == WM_LBUTTONUP {
                    spawn_self(&["--mode", "rect"]);
                } else if mouse_msg == WM_RBUTTONUP {
                    show_resident_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Executa o loop residente de bandeja + atalho global `Ctrl+Shift+S` (`RegisterHotKey`) no Windows 11.
    /// Input: nenhum | Output: nenhum (bloqueia até o usuário clicar em `Quit`).
    pub fn run_resident_tray_and_hotkey() {
        unsafe {
            let hmod = match GetModuleHandleW(None) {
                Ok(m) => m,
                Err(err) => {
                    warn!(error = %err, "GetModuleHandleW failed for Windows tray");
                    return;
                }
            };
            let hinst = windows::Win32::Foundation::HINSTANCE(hmod.0);
            let class_name = HSTRING::from("LxSnippingToolResidentTray");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(resident_wnd_proc),
                hInstance: hinst,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };
            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &class_name,
                &HSTRING::from("LX Snipping Tool Tray"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                HWND::default(),
                None,
                hinst,
                None,
            );
            if hwnd.0 == 0 {
                warn!("CreateWindowExW failed for Windows tray window");
                return;
            }

            if let Err(err) = RegisterHotKey(
                hwnd,
                HOTKEY_ID,
                MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                b'S' as u32,
            ) {
                warn!(error = %err, "RegisterHotKey(Ctrl+Shift+S) failed (shortcut may be in use)");
            }

            let (iw, ih, argb) = render_idle_badge_argb();
            let hicon = create_hicon_from_argb(iw, ih, &argb).unwrap_or_default();
            let mut nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAY_CALLBACK,
                hIcon: hicon,
                ..Default::default()
            };
            write_tip(&mut nid.szTip, "LX Snipping Tool (Ctrl+Shift+S)");
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            if hicon.0 != 0 {
                let _ = DestroyIcon(hicon);
            }
        }
    }

    /// Inicia o serviço residente de bandeja + `RegisterHotKey` em thread dedicada quando combinado com outro modo.
    /// Input: nenhum | Output: nenhum (`()`).
    pub fn spawn_tray_if_available() {
        std::thread::Builder::new()
            .name("win-tray-hotkey".to_string())
            .spawn(run_resident_tray_and_hotkey)
            .ok();
    }

    /// Estado compartilhado síncrono entre o loop Win32 da bandeja de gravação e o controlador assíncrono.
    /// Armazena: `elapsed_secs`, `paused`, `floating_bar_visible` e canal `tx` | Principais métodos: `label_text`, `pulse_bright`.
    struct WinRecTrayState {
        elapsed_secs: u64,
        paused: bool,
        floating_bar_visible: bool,
        tx: UnboundedSender<TrayRecordingCmd>,
    }

    impl WinRecTrayState {
        /// Retorna o texto formatado do cronômetro para tooltip do ícone.
        /// Input: `&self` | Output: `String` (`REC MM:SS` / `PAUSADO MM:SS`).
        fn label_text(&self) -> String {
            format_top_bar_label(self.elapsed_secs, self.paused)
        }

        /// Alterna brilho em segundos pares durante gravação ativa.
        /// Input: `&self` | Output: `bool`.
        fn pulse_bright(&self) -> bool {
            !self.paused && self.elapsed_secs.is_multiple_of(2)
        }
    }

    /// Estado interno associado via `GWLP_USERDATA` à janela da bandeja de gravação no Windows.
    /// Armazena: ponteiro compartilhado `state`, dados `nid` e último ícone `current_icon` | Principais métodos: nenhum (interno).
    struct WinRecWindowCtx {
        state: Arc<Mutex<WinRecTrayState>>,
        nid: NOTIFYICONDATAW,
        current_icon: HICON,
    }

    /// Exibe o menu de contexto do indicador ao vivo de gravação na bandeja do Windows.
    /// Input: `hwnd` e contexto `ctx` | Output: nenhum (`()`).
    unsafe fn show_recording_menu(hwnd: HWND, ctx: &WinRecWindowCtx) {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let (paused, bar_visible, tx) = {
            let g = ctx.state.lock().expect("tray mutex poisoned");
            (g.paused, g.floating_bar_visible, g.tx.clone())
        };
        let pause_lbl = if paused {
            "Retomar gravação"
        } else {
            "Pausar gravação"
        };
        let bar_lbl = if bar_visible {
            "Ocultar barra flutuante"
        } else {
            "Mostrar barra flutuante"
        };
        let _ = AppendMenuW(menu, MF_STRING, CMD_REC_PAUSE, &HSTRING::from(pause_lbl));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            CMD_REC_STOP,
            &HSTRING::from("Parar gravação"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_REC_TOGGLE_BAR, &HSTRING::from(bar_lbl));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 as usize {
            CMD_REC_PAUSE => {
                let _ = tx.send(TrayRecordingCmd::TogglePause);
            }
            CMD_REC_STOP => {
                let _ = tx.send(TrayRecordingCmd::Stop);
            }
            CMD_REC_TOGGLE_BAR => {
                let _ = tx.send(TrayRecordingCmd::ToggleFloatingBar);
            }
            _ => {}
        }
    }

    /// Procedure Win32 da bandeja ao vivo de gravação processando cliques e atualizações `WM_REC_REFRESH`.
    /// Input: parâmetros padrão `WNDPROC` Win32 | Output: `LRESULT`.
    unsafe extern "system" fn recording_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let ctx_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinRecWindowCtx;
        if ctx_ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ctx = &mut *ctx_ptr;

        match msg {
            WM_REC_REFRESH => {
                let (label, paused, pulse) = {
                    let g = ctx.state.lock().expect("tray mutex poisoned");
                    (g.label_text(), g.paused, g.pulse_bright())
                };
                let (iw, ih, argb) = render_tray_icon(paused, pulse);
                if let Some(new_icon) = create_hicon_from_argb(iw, ih, &argb) {
                    if ctx.current_icon.0 != 0 {
                        let _ = DestroyIcon(ctx.current_icon);
                    }
                    ctx.current_icon = new_icon;
                    ctx.nid.hIcon = new_icon;
                }
                write_tip(&mut ctx.nid.szTip, &format!("{label} — LX Snipping Tool"));
                let _ = Shell_NotifyIconW(NIM_MODIFY, &ctx.nid);
                LRESULT(0)
            }
            WM_TRAY_CALLBACK => {
                let mouse_msg = (lparam.0 & 0xFFFF) as u32;
                let tx = { ctx.state.lock().expect("tray mutex poisoned").tx.clone() };
                if mouse_msg == WM_LBUTTONDBLCLK {
                    let _ = tx.send(TrayRecordingCmd::Stop);
                } else if mouse_msg == WM_MBUTTONUP {
                    let _ = tx.send(TrayRecordingCmd::TogglePause);
                } else if mouse_msg == WM_RBUTTONUP {
                    show_recording_menu(hwnd, ctx);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Controla o ícone pulsante de gravação na bandeja do Windows e encerra sua thread com segurança.
    /// Armazena: `hwnd_raw`, `state` compartilhado e `thread` do loop Win32 | Principais métodos: `update`, `shutdown`.
    #[derive(Clone)]
    pub struct RecordingTrayHandle {
        hwnd_raw: isize,
        state: Arc<Mutex<WinRecTrayState>>,
        thread: Arc<Mutex<Option<JoinHandle<()>>>>,
    }

    impl RecordingTrayHandle {
        /// Atualiza cronômetro, estado de pausa e ícone pulsante na bandeja do Windows.
        /// Input: `&self`, `elapsed_secs`, `paused` e `floating_bar_visible` | Output: futuro sem retorno.
        pub async fn update(&self, elapsed_secs: u64, paused: bool, floating_bar_visible: bool) {
            if let Ok(mut g) = self.state.lock() {
                g.elapsed_secs = elapsed_secs;
                g.paused = paused;
                g.floating_bar_visible = floating_bar_visible;
            }
            if self.hwnd_raw != 0 {
                unsafe {
                    let _ = PostMessageW(HWND(self.hwnd_raw), WM_REC_REFRESH, WPARAM(0), LPARAM(0));
                }
            }
        }

        /// Solicita o fechamento da janela de bandeja, remove o ícone e aguarda o encerramento da thread Win32.
        /// Input: `&self` | Output: futuro concluído após o shutdown.
        pub async fn shutdown(&self) {
            if self.hwnd_raw != 0 {
                unsafe {
                    let _ = PostMessageW(HWND(self.hwnd_raw), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            let handle = self.thread.lock().ok().and_then(|mut guard| guard.take());
            if let Some(handle) = handle {
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = handle.join();
                })
                .await;
            }
        }
    }

    /// Inicia o indicador ao vivo de gravação na área de notificação do Windows 11 (`Shell_NotifyIconW`).
    /// Input: nenhum | Output: `Result<(RecordingTrayHandle, UnboundedReceiver<TrayRecordingCmd>), String>`.
    pub async fn spawn_recording_tray(
    ) -> Result<(RecordingTrayHandle, UnboundedReceiver<TrayRecordingCmd>), String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(WinRecTrayState {
            elapsed_secs: 0,
            paused: false,
            floating_bar_visible: false,
            tx,
        }));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize, String>>();
        let state_worker = Arc::clone(&state);

        let handle = std::thread::Builder::new()
            .name("win-rec-tray".to_string())
            .spawn(move || unsafe {
                let Ok(hmod) = GetModuleHandleW(None) else {
                    let _ = ready_tx.send(Err("GetModuleHandleW failed".to_string()));
                    return;
                };
                let hinst = windows::Win32::Foundation::HINSTANCE(hmod.0);
                let class_name = HSTRING::from("LxSnippingToolRecordingTray");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(recording_wnd_proc),
                    hInstance: hinst,
                    lpszClassName: PCWSTR(class_name.as_ptr()),
                    ..Default::default()
                };
                RegisterClassW(&wc);

                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    &class_name,
                    &HSTRING::from("LX Snipping Tool Recording"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    0,
                    0,
                    HWND::default(),
                    None,
                    hinst,
                    None,
                );
                if hwnd.0 == 0 {
                    let _ =
                        ready_tx.send(Err("CreateWindowExW failed for recording tray".to_string()));
                    return;
                }

                let (iw, ih, argb) = render_tray_icon(false, true);
                let icon = create_hicon_from_argb(iw, ih, &argb).unwrap_or_default();
                let mut nid = NOTIFYICONDATAW {
                    cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: 2,
                    uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                    uCallbackMessage: WM_TRAY_CALLBACK,
                    hIcon: icon,
                    ..Default::default()
                };
                write_tip(&mut nid.szTip, "REC 00:00 — LX Snipping Tool");
                let _ = Shell_NotifyIconW(NIM_ADD, &nid);

                let mut ctx = Box::new(WinRecWindowCtx {
                    state: state_worker,
                    nid,
                    current_icon: icon,
                });
                SetWindowLongPtrW(
                    hwnd,
                    GWLP_USERDATA,
                    (&mut *ctx as *mut WinRecWindowCtx) as isize,
                );
                let _ = ready_tx.send(Ok(hwnd.0));

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = Shell_NotifyIconW(NIM_DELETE, &ctx.nid);
                if ctx.current_icon.0 != 0 {
                    let _ = DestroyIcon(ctx.current_icon);
                }
            })
            .map_err(|e| format!("failed to spawn Windows recording tray thread: {e}"))?;

        let hwnd_raw = tokio::task::spawn_blocking(move || ready_rx.recv())
            .await
            .map_err(|e| format!("recording tray join failed: {e}"))?
            .map_err(|_| "recording tray thread exited prematurely".to_string())??;

        Ok((
            RecordingTrayHandle {
                hwnd_raw,
                state,
                thread: Arc::new(Mutex::new(Some(handle))),
            },
            rx,
        ))
    }
}

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use platform_tray::{
    spawn_recording_tray, spawn_tray_if_available, RecordingTrayHandle, SnippingTray,
};

#[cfg(target_os = "windows")]
#[allow(unused_imports)]
pub use platform_tray::{
    run_resident_tray_and_hotkey, spawn_recording_tray, spawn_tray_if_available,
    RecordingTrayHandle,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_pixmap_has_expected_argb32_size_and_pulses() {
        let (w, h, bright) = render_tray_icon(false, true);
        assert_eq!((w, h), (24, 24));
        assert_eq!(bright.len(), 24 * 24 * 4);

        let (_, _, dim) = render_tray_icon(false, false);
        assert_ne!(bright, dim);

        let (_, _, paused) = render_tray_icon(true, true);
        assert_eq!(paused.len(), 24 * 24 * 4);
        assert_ne!(bright, paused);
    }

    #[test]
    fn top_bar_label_formats_recording_and_paused() {
        assert_eq!(format_top_bar_label(5, false), "REC 00:05");
        assert_eq!(format_top_bar_label(5, true), "PAUSADO 00:05");
        assert_eq!(format_top_bar_label(125, false), "REC 02:05");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sni_direct_clicks_trigger_pause_and_stop() {
        use std::sync::{Arc, Mutex};
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(platform_tray::TrayState {
            elapsed_secs: 0,
            paused: false,
            floating_bar_visible: false,
            revision: 1,
            tx,
        }));
        let sni = platform_tray::RecordingSni { state };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio current_thread runtime builds");
        rt.block_on(async {
            sni.activate(0, 0).await.unwrap();
            sni.secondary_activate(0, 0).await.unwrap();
            sni.x_ayatana_secondary_activate(0).await.unwrap();
        });
        assert_eq!(rx.try_recv().unwrap(), TrayRecordingCmd::Stop);
        assert_eq!(rx.try_recv().unwrap(), TrayRecordingCmd::TogglePause);
        assert_eq!(rx.try_recv().unwrap(), TrayRecordingCmd::TogglePause);
        assert!(rx.try_recv().is_err());
    }
}
