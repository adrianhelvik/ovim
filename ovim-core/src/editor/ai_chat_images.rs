use super::Editor;
use crate::ai::chat_types::{ChatFocus, ImageAttachment};
use anyhow::{bail, Context, Result};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TOTAL_IMAGE_BYTES: usize = 40 * 1024 * 1024;

impl Editor {
    pub fn ai_chat_pending_images(&self) -> &[ImageAttachment] {
        self.ai_state
            .chat
            .as_ref()
            .map(|chat| chat.pending_images.as_slice())
            .unwrap_or(&[])
    }

    pub fn ai_chat_gallery_image_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .conversation()
            .into_iter()
            .flat_map(|conversation| conversation.messages().iter())
            .flat_map(|message| message.images.iter())
            .map(|image| image.path.clone())
            .collect::<Vec<_>>();
        paths.extend(
            self.ai_chat_pending_images()
                .iter()
                .map(|image| image.path.clone()),
        );
        let mut seen = std::collections::HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        paths
    }

    pub fn ai_chat_image_modal_path(&self) -> Option<&Path> {
        self.ai_state.chat.as_ref()?.image_modal.as_deref()
    }

    pub fn open_ai_chat_image_modal(&mut self, path: PathBuf) {
        if let Some(chat) = self.ai_state.chat.as_mut() {
            chat.image_modal = Some(path);
        }
    }

    pub fn close_ai_chat_image_modal(&mut self) -> bool {
        self.ai_state
            .chat
            .as_mut()
            .and_then(|chat| chat.image_modal.take())
            .is_some()
    }

    /// Interpret a bracketed paste consisting entirely of image paths as a
    /// terminal drag/drop. Ordinary text and mixed text/path pastes remain
    /// ordinary composer input.
    pub(crate) fn try_attach_dropped_chat_images(&mut self, text: &str) -> Result<bool> {
        let Some(chat) = self.ai_state.chat.as_ref() else {
            return Ok(false);
        };
        if chat.focus != ChatFocus::TextInput {
            return Ok(false);
        }

        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(false);
        }

        // Headless clients send paste payloads literally, so a valid single
        // image path containing spaces should not need shell escaping. Keep
        // shlex parsing as the fallback for terminal drag/drop and batches.
        let paths = dropped_path(trimmed)
            .filter(|path| path.is_file() && image_mime_type(path).is_some())
            .map(|path| vec![path])
            .or_else(|| {
                let tokens = shlex::split(trimmed)?;
                if tokens.is_empty() {
                    return None;
                }
                let paths: Vec<PathBuf> = tokens
                    .iter()
                    .filter_map(|token| dropped_path(token))
                    .collect();
                (paths.len() == tokens.len()).then_some(paths)
            });
        let Some(paths) = paths else {
            return Ok(false);
        };
        if paths
            .iter()
            .any(|path| !path.is_file() || image_mime_type(path).is_none())
        {
            return Ok(false);
        }

        let mut images = Vec::with_capacity(paths.len());
        for path in paths {
            images.push(load_image(path)?);
        }
        let existing_bytes: usize = self
            .ai_chat_pending_images()
            .iter()
            .map(|image| image.data.len())
            .sum();
        let new_bytes: usize = images.iter().map(|image| image.data.len()).sum();
        if existing_bytes.saturating_add(new_bytes) > MAX_TOTAL_IMAGE_BYTES {
            bail!("Chat image attachments exceed the 40 MiB total limit");
        }

        let mut added = 0usize;
        if let Some(chat) = self.ai_state.chat.as_mut() {
            for image in images {
                if chat
                    .pending_images
                    .iter()
                    .any(|existing| existing.path == image.path)
                {
                    continue;
                }
                chat.pending_images.push(image);
                added += 1;
            }
        }
        if added > 0 {
            self.set_status_message(format!(
                "Attached {added} image{}",
                if added == 1 { "" } else { "s" }
            ));
        }
        Ok(true)
    }

    /// Attach image bytes supplied by a graphical clipboard without requiring
    /// a filesystem path. The synthetic path is stable for identical content
    /// and retains the extension used by provider and gallery projections.
    pub fn attach_ai_chat_image_data(&mut self, file_name: &str, data: Vec<u8>) -> Result<()> {
        if self.has_codex_auth_dialog() || self.ai_chat_has_exa_setup_dialog() {
            bail!("Finish or dismiss AI setup before pasting an image");
        }
        let Some(chat) = self.ai_state.chat.as_ref() else {
            bail!("Open AI chat before pasting an image");
        };
        if chat.focus != ChatFocus::TextInput {
            bail!("Focus the AI chat composer before pasting an image");
        }
        if data.len() as u64 > MAX_IMAGE_BYTES {
            bail!("Image exceeds the 20 MiB limit");
        }
        let source_path = Path::new(file_name);
        let mime_type = image_mime_type(source_path).context("Unsupported image format")?;
        if !has_image_signature(mime_type, &data) {
            bail!("Clipboard data is not a valid {mime_type} image");
        }
        let existing_bytes: usize = self
            .ai_chat_pending_images()
            .iter()
            .map(|image| image.data.len())
            .sum();
        if existing_bytes.saturating_add(data.len()) > MAX_TOTAL_IMAGE_BYTES {
            bail!("Chat image attachments exceed the 40 MiB total limit");
        }

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        file_name.hash(&mut hasher);
        data.hash(&mut hasher);
        let extension = source_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("png");
        let path = PathBuf::from(format!(
            "clipboard-{:016x}.{}",
            hasher.finish(),
            extension.to_ascii_lowercase()
        ));
        let image = ImageAttachment {
            path: path.clone(),
            mime_type: mime_type.to_string(),
            data,
        };
        if let Some(chat) = self.ai_state.chat.as_mut() {
            if chat
                .pending_images
                .iter()
                .any(|existing| existing.path == path)
            {
                return Ok(());
            }
            chat.pending_images.push(image);
        }
        self.set_status_message("Attached clipboard image".to_string());
        Ok(())
    }

    /// Backspace on an empty composer removes the most recently attached image.
    pub fn remove_last_ai_chat_image(&mut self) -> bool {
        let Some(last) = self.ai_chat_pending_images().len().checked_sub(1) else {
            return false;
        };
        self.remove_ai_chat_image(last)
    }

    /// Removes a pending composer image by its displayed index.
    pub fn remove_ai_chat_image(&mut self, index: usize) -> bool {
        let Some(chat) = self.ai_state.chat.as_mut() else {
            return false;
        };
        if index >= chat.pending_images.len() {
            return false;
        }
        chat.pending_images.remove(index);
        true
    }
}

fn dropped_path(token: &str) -> Option<PathBuf> {
    let path = if token.starts_with("file://") {
        url::Url::parse(token).ok()?.to_file_path().ok()?
    } else {
        PathBuf::from(token)
    };
    Some(path.canonicalize().unwrap_or(path))
}

fn image_mime_type(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub(crate) fn load_image(path: PathBuf) -> Result<ImageAttachment> {
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("Failed to inspect dropped image: {}", path.display()))?;
    if metadata.len() > MAX_IMAGE_BYTES {
        bail!("Image exceeds the 20 MiB limit: {}", path.display());
    }
    let mime_type = image_mime_type(&path).context("Unsupported image format")?;
    let data = std::fs::read(&path)
        .with_context(|| format!("Failed to read dropped image: {}", path.display()))?;
    if !has_image_signature(mime_type, &data) {
        bail!(
            "Dropped file is not a valid {mime_type} image: {}",
            path.display()
        );
    }
    Ok(ImageAttachment {
        path,
        mime_type: mime_type.to_string(),
        data,
    })
}

fn has_image_signature(mime_type: &str, data: &[u8]) -> bool {
    match mime_type {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
        "image/webp" => data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat_types::ChatOpts;

    #[test]
    fn image_path_paste_attaches_instead_of_typing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("screen shot.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nminimal").unwrap();
        let escaped = path.to_string_lossy().replace(' ', "\\ ");

        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        assert!(editor.try_attach_dropped_chat_images(&escaped).unwrap());
        assert!(editor.ai_chat_input().is_empty());
        assert_eq!(editor.ai_chat_pending_images().len(), 1);
        assert_eq!(
            editor.ai_chat_pending_images()[0].file_name(),
            "screen shot.png"
        );
    }

    #[test]
    fn literal_image_path_with_spaces_attaches_for_headless_paste() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("literal screen shot.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nminimal").unwrap();

        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        assert!(editor
            .try_attach_dropped_chat_images(path.to_string_lossy().as_ref())
            .unwrap());
        assert!(editor.ai_chat_input().is_empty());
        assert_eq!(editor.ai_chat_pending_images().len(), 1);
        assert_eq!(
            editor.ai_chat_pending_images()[0].file_name(),
            "literal screen shot.png"
        );
    }

    #[test]
    fn ordinary_path_text_remains_a_normal_paste() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        assert!(!editor
            .try_attach_dropped_chat_images("please inspect image.png")
            .unwrap());
        assert!(editor.ai_chat_pending_images().is_empty());
    }

    #[test]
    fn clipboard_image_bytes_are_validated_and_attached() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();

        editor
            .attach_ai_chat_image_data("pasted.png", b"\x89PNG\r\n\x1a\nminimal".to_vec())
            .unwrap();

        let image = &editor.ai_chat_pending_images()[0];
        assert_eq!(image.mime_type, "image/png");
        assert!(image.file_name().starts_with("clipboard-"));
        assert!(editor
            .attach_ai_chat_image_data("fake.png", b"not an image".to_vec())
            .is_err());
        assert_eq!(editor.ai_chat_pending_images().len(), 1);
    }

    #[test]
    fn gallery_paths_are_deduplicated_and_modal_can_be_closed() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let path = PathBuf::from("/tmp/image.png");
        let chat = editor.ai_state.chat.as_mut().unwrap();
        for _ in 0..2 {
            chat.pending_images.push(ImageAttachment {
                path: path.clone(),
                mime_type: "image/png".into(),
                data: vec![],
            });
        }

        assert_eq!(editor.ai_chat_gallery_image_paths(), vec![path.clone()]);
        editor.open_ai_chat_image_modal(path.clone());
        assert_eq!(editor.ai_chat_image_modal_path(), Some(path.as_path()));
        assert!(editor.close_ai_chat_image_modal());
        assert!(editor.ai_chat_image_modal_path().is_none());
    }

    #[test]
    fn pending_images_can_be_removed_by_displayed_index() {
        let mut editor = Editor::default();
        editor.open_ai_chat(ChatOpts::default()).unwrap();
        let chat = editor.ai_state.chat.as_mut().unwrap();
        for name in ["first.png", "second.png"] {
            chat.pending_images.push(ImageAttachment {
                path: PathBuf::from(name),
                mime_type: "image/png".into(),
                data: vec![],
            });
        }

        assert!(editor.remove_ai_chat_image(0));
        assert_eq!(editor.ai_chat_pending_images()[0].file_name(), "second.png");
        assert!(!editor.remove_ai_chat_image(1));
        assert!(editor.remove_last_ai_chat_image());
        assert!(editor.ai_chat_pending_images().is_empty());
    }
}
