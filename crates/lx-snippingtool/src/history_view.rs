//! Snip history view displaying bounded `snips/` thumbnails with Copy, Edit, and Delete actions.

use std::path::PathBuf;

use iced::widget::{button, column, container, image, row, scrollable, text, Column};
use iced::{Alignment, Element, Length};
use lx_snip_core::storage::{SnipEntry, SnipStore};

/// Messages emitted by [`HistoryState`].
#[derive(Debug, Clone)]
pub enum HistoryMessage {
    /// Reload snip entries from disk.
    Refresh,
    /// Copy the selected snip PNG bytes to the clipboard and exit UI.
    Copy(PathBuf),
    /// Open the selected snip in the annotation editor.
    Edit(PathBuf),
    /// Delete the selected snip from disk.
    Delete(PathBuf),
}

/// External action requested by [`HistoryState::update`].
#[derive(Debug, Clone)]
pub enum HistoryAction {
    /// Copy raw PNG bytes to clipboard and close UI.
    CopyPng {
        /// Path to the snip file.
        path: PathBuf,
        /// Raw PNG bytes.
        png_bytes: Vec<u8>,
    },
    /// Open the given snip file in the annotation editor.
    OpenEditor(PathBuf),
}

/// Entry paired with an `iced::widget::image::Handle` thumbnail.
#[derive(Debug, Clone)]
pub struct HistoryItem {
    /// Underlying storage entry.
    pub entry: SnipEntry,
    /// Thumbnail handle for `iced::widget::image`.
    pub handle: image::Handle,
}

/// State of the Snip History window.
#[derive(Debug, Clone)]
pub struct HistoryState {
    store: SnipStore,
    items: Vec<HistoryItem>,
    status_text: Option<String>,
}

impl HistoryState {
    /// Creates a new [`HistoryState`] backed by `store` and loads existing entries.
    pub fn new(store: SnipStore) -> Self {
        let mut state = Self {
            store,
            items: Vec::new(),
            status_text: None,
        };
        state.reload();
        state
    }

    /// Reloads snip entries from `store`.
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

    /// Updates history state and optionally returns a [`HistoryAction`].
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

    /// Renders the Snip History UI.
    pub fn view(&self) -> Element<'_, HistoryMessage> {
        let header = row![
            text(format!("Snip History ({})", self.items.len())).size(18),
            button(text("Refresh").size(13))
                .padding([6, 12])
                .on_press(HistoryMessage::Refresh),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

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

                let meta = column![
                    text(name).size(14),
                    text(format!("{kb:.1} KiB")).size(12),
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
                ]
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
