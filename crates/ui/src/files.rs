//! M4 File Manager window (browse remote directories, upload/download with resume & zstd,
//! mkdir, rename, delete).

use veloxdesk_protocol::control::FileEntry;

use crate::i18n::tr;
use crate::settings::Language;

/// Actions emitted by [`FileManagerWindow`] to the viewer session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileAction {
    ListDir(String),
    Mkdir(String),
    Delete(String),
    Rename { from: String, to: String },
    Download { remote_path: String, local_path: String, resume: bool, compress: bool },
    Upload { local_path: String, remote_path: String, resume: bool, compress: bool },
}

/// Active transfer progress displayed in the file manager footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub label: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

impl TransferProgress {
    pub fn fraction(&self) -> f32 {
        if self.bytes_total == 0 {
            0.0
        } else {
            (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// Format byte sizes cleanly (`1.4 MiB`, `820 B`, etc.).
pub fn format_size_i18n(bytes: u64, lang: Language) -> String {
    tr(lang).format_size(bytes)
}

/// Format byte sizes cleanly (`1.4 MiB`, `820 B`, etc.) in English.
pub fn format_size(bytes: u64) -> String {
    format_size_i18n(bytes, Language::En)
}

/// Join a remote directory path and entry name without platform-specific host assumptions.
pub fn join_remote_path(dir: &str, name: &str) -> String {
    let d = dir.trim();
    if d.is_empty() || d == "." {
        return name.to_string();
    }
    let sep = if d.contains('\\') && !d.contains('/') { '\\' } else { '/' };
    if d.ends_with('/') || d.ends_with('\\') {
        format!("{d}{name}")
    } else {
        format!("{d}{sep}{name}")
    }
}

/// Parent directory of a remote path string.
pub fn parent_remote_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed == "." {
        return ".".into();
    }
    match trimmed.rfind(['/', '\\']) {
        Some(0) => "/".into(),
        Some(idx) => {
            let prefix = &trimmed[..idx];
            if prefix.ends_with(':') {
                format!("{prefix}\\")
            } else {
                prefix.to_string()
            }
        }
        None => ".".into(),
    }
}

/// Remote File Manager egui window.
#[derive(Debug, Clone)]
pub struct FileManagerWindow {
    pub open: bool,
    pub current_path: String,
    pub entries: Vec<FileEntry>,
    pub selected: Option<usize>,
    pub local_path: String,
    pub new_name: String,
    pub resume: bool,
    pub compress: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    pub progress: Option<TransferProgress>,
}

impl Default for FileManagerWindow {
    fn default() -> Self {
        Self {
            open: false,
            current_path: ".".into(),
            entries: Vec::new(),
            selected: None,
            local_path: String::new(),
            new_name: String::new(),
            resume: true,
            compress: true,
            status: None,
            error: None,
            progress: None,
        }
    }
}

impl FileManagerWindow {
    pub fn show(&mut self, ctx: &egui::Context) -> Vec<FileAction> {
        self.show_for(ctx, Language::En)
    }

    pub fn show_for(&mut self, ctx: &egui::Context, lang: Language) -> Vec<FileAction> {
        let t = tr(lang);
        let mut actions = Vec::new();
        let mut open = self.open;
        egui::Window::new(t.files_title)
            .id(egui::Id::unique("veloxdesk-files"))
            .open(&mut open)
            .default_size(egui::vec2(660.0, 430.0))
            .resizable(true)
            .show(ctx, |ui| {
                // Path navigation bar.
                ui.horizontal(|ui| {
                    if ui.button((crate::icons::arrow_up_atom(12.0), t.files_up_btn)).clicked() {
                        self.current_path = parent_remote_path(&self.current_path);
                        self.selected = None;
                        actions.push(FileAction::ListDir(self.current_path.clone()));
                    }
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.current_path)
                            .desired_width((ui.available_width() - 96.0).max(120.0)),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(t.refresh_btn).clicked() || enter {
                        self.selected = None;
                        actions.push(FileAction::ListDir(self.current_path.clone()));
                    }
                });
                ui.separator();

                // Directory listing table.
                egui::ScrollArea::vertical().max_height(210.0).show(ui, |ui| {
                    egui::Grid::new("veloxdesk-file-list")
                        .num_columns(3)
                        .striped(true)
                        .min_col_width(80.0)
                        .show(ui, |ui| {
                            ui.strong(t.files_col_name);
                            ui.strong(t.files_col_type);
                            ui.strong(t.files_col_size);
                            ui.end_row();
                            let mut nav_dir: Option<String> = None;
                            for (idx, entry) in self.entries.iter().enumerate() {
                                use egui::IntoAtoms as _;
                                let is_sel = self.selected == Some(idx);
                                let resp = if entry.is_dir {
                                    ui.selectable_label(
                                        is_sel,
                                        (crate::icons::folder_atom(13.0), entry.name.as_str()).into_atoms(),
                                    )
                                } else {
                                    ui.selectable_label(is_sel, entry.name.as_str())
                                };
                                if resp.clicked() {
                                    self.selected = Some(idx);
                                }
                                if resp.double_clicked() && entry.is_dir {
                                    nav_dir = Some(join_remote_path(&self.current_path, &entry.name));
                                }
                                ui.label(if entry.is_dir { t.files_type_dir } else { t.files_type_file });
                                ui.label(if entry.is_dir {
                                    String::new()
                                } else {
                                    format_size_i18n(entry.size, lang)
                                });
                                ui.end_row();
                            }
                            if let Some(dir) = nav_dir {
                                self.current_path = dir.clone();
                                self.selected = None;
                                actions.push(FileAction::ListDir(dir));
                            }
                        });
                });

                ui.separator();

                // Transfer controls (Upload / Download + resume & zstd toggles).
                ui.horizontal_wrapped(|ui| {
                    ui.label(t.files_local_path_label);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.local_path)
                            .hint_text(t.files_local_path_hint)
                            .desired_width(240.0),
                    );
                    ui.checkbox(&mut self.resume, t.files_resume_cb);
                    ui.checkbox(&mut self.compress, "zstd");
                });
                ui.horizontal_wrapped(|ui| {
                    let selected_entry = self.selected.and_then(|i| self.entries.get(i)).cloned();
                    let can_download = selected_entry.as_ref().is_some_and(|e| !e.is_dir)
                        && !self.local_path.trim().is_empty();
                    if ui.add_enabled(can_download, egui::Button::new(t.files_download_btn)).clicked()
                        && let Some(e) = &selected_entry
                    {
                        actions.push(FileAction::Download {
                            remote_path: join_remote_path(&self.current_path, &e.name),
                            local_path: self.local_path.trim().to_string(),
                            resume: self.resume,
                            compress: self.compress,
                        });
                    }
                    let can_upload = !self.local_path.trim().is_empty();
                    if ui.add_enabled(can_upload, egui::Button::new(t.files_upload_btn)).clicked() {
                        let local = self.local_path.trim();
                        let fname = std::path::Path::new(local)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("upload.bin");
                        actions.push(FileAction::Upload {
                            local_path: local.to_string(),
                            remote_path: join_remote_path(&self.current_path, fname),
                            resume: self.resume,
                            compress: self.compress,
                        });
                    }
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_name)
                            .hint_text(t.files_new_name_hint)
                            .desired_width(140.0),
                    );
                    if ui
                        .add_enabled(!self.new_name.trim().is_empty(), egui::Button::new(t.files_mkdir_btn))
                        .clicked()
                    {
                        actions.push(FileAction::Mkdir(join_remote_path(&self.current_path, self.new_name.trim())));
                        self.new_name.clear();
                    }
                    let can_rename = selected_entry.is_some() && !self.new_name.trim().is_empty();
                    if ui.add_enabled(can_rename, egui::Button::new(t.files_rename_btn)).clicked()
                        && let Some(e) = &selected_entry
                    {
                        actions.push(FileAction::Rename {
                            from: join_remote_path(&self.current_path, &e.name),
                            to: join_remote_path(&self.current_path, self.new_name.trim()),
                        });
                        self.new_name.clear();
                    }
                    if ui
                        .add_enabled(
                            selected_entry.is_some(),
                            egui::Button::new(
                                egui::RichText::new(t.files_delete_btn).color(egui::Color32::from_rgb(255, 120, 120)),
                            ),
                        )
                        .clicked()
                        && let Some(e) = &selected_entry
                    {
                        actions.push(FileAction::Delete(join_remote_path(&self.current_path, &e.name)));
                        self.selected = None;
                    }
                });

                if let Some(p) = &self.progress {
                    ui.add_space(4.0);
                    let text = format!(
                        "{} ({} / {})",
                        p.label,
                        format_size_i18n(p.bytes_done, lang),
                        format_size_i18n(p.bytes_total, lang)
                    );
                    ui.add(egui::ProgressBar::new(p.fraction()).text(text));
                }
                if let Some(s) = &self.status {
                    ui.weak(s);
                }
                if let Some(e) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(255, 110, 110), e);
                }
            });
        self.open = open;
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_and_size_helpers() {
        assert_eq!(join_remote_path(".", "notes.txt"), "notes.txt");
        assert_eq!(join_remote_path("/home/user", "docs"), "/home/user/docs");
        assert_eq!(join_remote_path("C:\\Users", "Desktop"), "C:\\Users\\Desktop");
        assert_eq!(parent_remote_path("/home/user/docs"), "/home/user");
        assert_eq!(parent_remote_path("C:\\Users"), "C:\\");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KiB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.00 MiB");
    }

    #[test]
    fn file_manager_renders_headless() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };
        let mut fm = FileManagerWindow {
            open: true,
            entries: vec![
                FileEntry { name: "subdir".into(), is_dir: true, size: 0, modified_unix: 0 },
                FileEntry { name: "readme.md".into(), is_dir: false, size: 4096, modified_unix: 0 },
            ],
            progress: Some(TransferProgress { label: "Downloading".into(), bytes_done: 1024, bytes_total: 4096 }),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            assert!(fm.show(ui.ctx()).is_empty());
        });
        out.textures_delta.clear();
    }
}
