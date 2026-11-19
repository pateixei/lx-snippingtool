//! Módulo: `history_view`
//! Objetivos: Exibir o histórico local limitado de capturas PNG com miniaturas e ações de Copiar, Editar e Excluir.
//! Principais itens: `HistoryMessage`, `HistoryAction`, `HistoryItem` e `HistoryState`.
//! Última alteração: 2026-11-19

use std::path::PathBuf;

use iced::widget::{button, column, container, image, row, scrollable, text, Column};
use iced::{Alignment, Element, Length};
use lx_snip_core::storage::{SnipEntry, SnipStore};

/// Mensagens de interação emitidas pela janela de histórico de capturas.
/// Armazena: variantes `Refresh`, `Copy`, `Edit` e `Delete` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone)]
pub enum HistoryMessage {
    /// Recarrega a lista de arquivos do disco.
    Refresh,
    /// Copia o PNG selecionado para a área de transferência e encerra a interface.
    Copy(PathBuf),
    /// Abre a captura selecionada no editor de anotações.
    Edit(PathBuf),
    /// Remove do disco a captura selecionada.
    Delete(PathBuf),
}

/// Ações externas solicitadas ao controlador principal por `HistoryState::update`.
/// Armazena: variantes `CopyPng` e `OpenEditor` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone)]
pub enum HistoryAction {
    /// Copia bytes PNG crus para o clipboard e fecha a janela.
    CopyPng {
        /// Caminho do arquivo de captura.
        path: PathBuf,
        /// Conteúdo bruto do PNG.
        png_bytes: Vec<u8>,
    },
    /// Abre o arquivo informado no editor de anotações.
    OpenEditor(PathBuf),
}

/// Associa uma entrada do repositório em disco ao respectivo `image::Handle` de miniatura.
/// Armazena: `entry` e `handle` | Principais métodos: nenhum (tipo de dados).
#[derive(Debug, Clone)]
pub struct HistoryItem {
    /// Entrada subjacente no `SnipStore`.
    pub entry: SnipEntry,
    /// Handle de imagem usado para exibir a miniatura no Iced.
    pub handle: image::Handle,
}

/// Mantém o estado e a renderização da janela de histórico de capturas.
/// Armazena: `store`, lista `items` e `status_text` | Principais métodos: `new`, `reload`, `update`, `view`.
#[derive(Debug, Clone)]
pub struct HistoryState {
    store: SnipStore,
    items: Vec<HistoryItem>,
    status_text: Option<String>,
}

impl HistoryState {
    /// Cria um `HistoryState` conectado a `store` e já carrega os itens do disco.
    /// Input: `store` do tipo `SnipStore` | Output: instância inicializada de `HistoryState`.
    pub fn new(store: SnipStore) -> Self {
        let mut state = Self {
            store,
            items: Vec::new(),
            status_text: None,
        };
        state.reload();
        state
    }

    /// Releitura das capturas salvas em `store` para atualizar miniaturas e status.
    /// Input: `&mut self` | Output: atualiza `self.items` e `self.status_text`.
    pub fn reload(&mut self) {
        match self.store.list() {
            Ok(entries) => {
                self.items = entries
                    .into_iter()
                    .map(|entry| {
                        let handle = image::Handle::from_path(&entry.path);
                        HistoryItem { entry, handle }
                    })
                    .collect();
                self.status_text = None;
            }
            Err(err) => {
                self.items.clear();
                self.status_text = Some(format!("Failed to load snips: {err}"));
            }
        }
    }

    /// Processa uma `HistoryMessage` e retorna uma `HistoryAction` quando exige ação da janela principal.
    /// Input: `&mut self` e `msg` | Output: `Option<HistoryAction>`.
    pub fn update(&mut self, msg: HistoryMessage) -> Option<HistoryAction> {
        match msg {
            HistoryMessage::Refresh => {
                self.reload();
                None
            }
            HistoryMessage::Copy(path) => match std::fs::read(&path) {
                Ok(png_bytes) => Some(HistoryAction::CopyPng { path, png_bytes }),
                Err(err) => {
                    self.status_text = Some(format!("Failed to read {}: {err}", path.display()));
                    None
                }
            },
            HistoryMessage::Edit(path) => Some(HistoryAction::OpenEditor(path)),
            HistoryMessage::Delete(path) => {
                if let Err(err) = self.store.delete(&path) {
                    self.status_text = Some(format!("Failed to delete: {err}"));
                } else {
                    self.reload();
                }
                None
            }
        }
    }

    /// Constrói a árvore de widgets Iced da lista de histórico e seus botões de ação.
    /// Input: `&self` | Output: `Element<'_, HistoryMessage>`.
    pub fn view(&self) -> Element<'_, HistoryMessage> {
        let header = container(
            row![
                text(format!("Snip History ({})", self.items.len())).size(18),
                button(text("Refresh").size(13))
                    .padding([6, 12])
                    .on_press(HistoryMessage::Refresh),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .center_x(Length::Fill);

        let mut list_col = Column::new().spacing(10).width(Length::Fill);
        if self.items.is_empty() {
            list_col = list_col.push(
                container(text("No snips saved yet.").size(14))
                    .padding(24)
                    .center_x(Length::Fill),
            );
        } else {
            for item in &self.items {
                let name = item
                    .entry
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("snip.png")
                    .to_string();
                let kb = (item.entry.byte_size as f64) / 1024.0;

                let thumb = image(item.handle.clone())
                    .width(Length::Fixed(160.0))
                    .height(Length::Fixed(100.0))
                    .content_fit(iced::ContentFit::Contain);

                let actions = container(
                    row![
                        button(text("Copy").size(12))
                            .padding([4, 10])
                            .style(button::primary)
                            .on_press(HistoryMessage::Copy(item.entry.path.clone())),
                        button(text("Edit").size(12))
                            .padding([4, 10])
                            .on_press(HistoryMessage::Edit(item.entry.path.clone())),
                        button(text("Delete").size(12))
                            .padding([4, 10])
                            .style(button::danger)
                            .on_press(HistoryMessage::Delete(item.entry.path.clone())),
                    ]
                    .spacing(6),
                )
                .width(Length::Fill)
                .center_x(Length::Fill);

                let meta = column![
                    text(name).size(14),
                    text(format!("{kb:.1} KiB")).size(12),
                    actions,
                ]
                .width(Length::Fill)
                .spacing(6);

                let card = container(row![thumb, meta].spacing(14).align_y(Alignment::Center))
                    .padding(10)
                    .width(Length::Fill);

                list_col = list_col.push(card);
            }
        }

        let mut root = Column::new().push(header).spacing(12).padding(16);
        if let Some(status) = &self.status_text {
            root = root.push(text(status.as_str()).size(12));
        }
        root = root.push(scrollable(list_col).height(Length::Fill));

        container(root)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}
