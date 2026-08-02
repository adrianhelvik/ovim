use crate::editor::Editor;
use image::ImageReader;
use ratatui::{layout::Rect, Frame};
use ratatui_image::picker::ProtocolType;
use ratatui_image::{picker::Picker, protocol::Protocol, Image, Resize};
use std::collections::HashMap;
#[cfg(not(test))]
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    path: PathBuf,
    width: u16,
    height: u16,
}

fn stable_auto_protocol(term_program: Option<&str>) -> Option<ProtocolType> {
    term_program
        .is_some_and(|program| program.eq_ignore_ascii_case("ghostty"))
        .then_some(ProtocolType::Halfblocks)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ImageProtocolPreference {
    Auto,
    Off,
    Force(ProtocolType),
}

fn image_protocol_preference(value: Option<&str>) -> ImageProtocolPreference {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("off" | "none" | "0") => ImageProtocolPreference::Off,
        Some("halfblocks" | "halfblock" | "cells") => {
            ImageProtocolPreference::Force(ProtocolType::Halfblocks)
        }
        Some("kitty") => ImageProtocolPreference::Force(ProtocolType::Kitty),
        Some("sixel") => ImageProtocolPreference::Force(ProtocolType::Sixel),
        Some("iterm2" | "iterm") => ImageProtocolPreference::Force(ProtocolType::Iterm2),
        _ => ImageProtocolPreference::Auto,
    }
}

pub struct TerminalImageRenderer {
    picker: Option<Picker>,
    fallback_picker: Option<Picker>,
    protocols: HashMap<CacheKey, Protocol>,
    rendered_last_frame: bool,
}

impl TerminalImageRenderer {
    pub fn detect() -> Self {
        #[cfg(test)]
        return Self {
            picker: None,
            fallback_picker: None,
            protocols: HashMap::new(),
            rendered_last_frame: false,
        };

        #[cfg(not(test))]
        {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                return Self {
                    picker: None,
                    fallback_picker: None,
                    protocols: HashMap::new(),
                    rendered_last_frame: false,
                };
            }
            // Prefer the advertised native graphics protocol. Halfblocks are a
            // terminal-independent fallback, so unsupported terminals still
            // get stable thumbnails instead of empty image boxes.
            let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
            match image_protocol_preference(std::env::var("OVIM_IMAGE_PROTOCOL").ok().as_deref()) {
                ImageProtocolPreference::Auto => {
                    // Ghostty's Kitty virtual placements can flicker under a
                    // frequently redrawn TUI. Prefer stable cell graphics by
                    // default; users can still opt into Kitty explicitly.
                    if let Some(protocol) =
                        stable_auto_protocol(std::env::var("TERM_PROGRAM").ok().as_deref())
                    {
                        picker.set_protocol_type(protocol);
                    }
                }
                ImageProtocolPreference::Off => {
                    return Self {
                        picker: None,
                        fallback_picker: None,
                        protocols: HashMap::new(),
                        rendered_last_frame: false,
                    };
                }
                ImageProtocolPreference::Force(protocol_type) => {
                    picker.set_protocol_type(protocol_type);
                }
            }
            let fallback_picker =
                (picker.protocol_type() != ProtocolType::Halfblocks).then(Picker::halfblocks);
            Self {
                picker: Some(picker),
                fallback_picker,
                protocols: HashMap::new(),
                rendered_last_frame: false,
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.picker.is_some()
    }

    pub fn uses_terminal_owned_images(&self) -> bool {
        self.picker
            .as_ref()
            .is_some_and(|picker| picker.protocol_type() != ProtocolType::Halfblocks)
    }

    pub fn rendered_last_frame(&self) -> bool {
        self.rendered_last_frame
    }

    /// Cursor-positioned iTerm and Sixel images can flicker when the hardware
    /// cursor is repeatedly repositioned. A software cursor keeps the composer
    /// usable without disturbing those terminal-owned placements.
    pub fn requires_software_cursor(&self) -> bool {
        self.picker.as_ref().is_some_and(|picker| {
            matches!(
                picker.protocol_type(),
                ProtocolType::Iterm2 | ProtocolType::Sixel
            )
        })
    }

    pub fn render(&mut self, frame: &mut Frame, editor: &Editor) {
        self.rendered_last_frame = false;
        if self.picker.is_none() {
            return;
        }

        let thumbnails = editor.render_cache.ai_chat_image_thumbnails.clone();
        for (area, path) in thumbnails {
            self.rendered_last_frame |= self.render_path(frame, &path, core_rect(area));
        }

        if let Some(path) = editor.ai_chat_image_modal_path() {
            let full = frame.area();
            if full.width >= 20 && full.height >= 10 {
                let outer_width = full.width * 4 / 5;
                let outer_height = full.height * 4 / 5;
                let area = Rect::new(
                    full.x + full.width.saturating_sub(outer_width) / 2 + 1,
                    full.y + full.height.saturating_sub(outer_height) / 2 + 1,
                    outer_width.saturating_sub(2),
                    outer_height.saturating_sub(2),
                );
                self.rendered_last_frame |= self.render_path(frame, path, area);
            }
        }

        // Keep long-running chats from retaining a decoded protocol for every
        // image/size combination ever shown.
        if self.protocols.len() > 64 {
            self.protocols.clear();
        }
    }

    fn render_path(&mut self, frame: &mut Frame, path: &Path, area: Rect) -> bool {
        if area.width == 0 || area.height == 0 {
            return false;
        }
        let key = CacheKey {
            path: path.to_path_buf(),
            width: area.width,
            height: area.height,
        };
        if !self.protocols.contains_key(&key) {
            let Some(picker) = self.picker.as_ref() else {
                return false;
            };
            let Ok(reader) = ImageReader::open(path) else {
                return false;
            };
            let Ok(image) = reader.decode() else {
                return false;
            };
            let size = Rect::new(0, 0, area.width, area.height);
            let protocol = match picker.new_protocol(image.clone(), size, Resize::Fit(None)) {
                Ok(protocol) => protocol,
                Err(_) => {
                    let Some(fallback) = self.fallback_picker.as_ref() else {
                        return false;
                    };
                    let Ok(protocol) = fallback.new_protocol(image, size, Resize::Fit(None)) else {
                        return false;
                    };
                    protocol
                }
            };
            self.protocols.insert(key.clone(), protocol);
        }
        if let Some(protocol) = self.protocols.get(&key) {
            frame.render_widget(Image::new(protocol), area);
            true
        } else {
            false
        }
    }
}

fn core_rect(area: ovim_core::Rect) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer_for(protocol_type: ProtocolType) -> TerminalImageRenderer {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(protocol_type);
        TerminalImageRenderer {
            picker: Some(picker),
            fallback_picker: Some(Picker::halfblocks()),
            protocols: HashMap::new(),
            rendered_last_frame: false,
        }
    }

    #[test]
    fn iterm_images_request_a_software_cursor() {
        assert!(renderer_for(ProtocolType::Iterm2).requires_software_cursor());
        assert!(!renderer_for(ProtocolType::Kitty).requires_software_cursor());
        assert!(renderer_for(ProtocolType::Sixel).requires_software_cursor());
        assert!(!renderer_for(ProtocolType::Halfblocks).requires_software_cursor());
    }

    #[test]
    fn halfblock_fallback_remains_an_enabled_renderer() {
        let renderer = renderer_for(ProtocolType::Halfblocks);
        assert!(renderer.is_enabled());
        assert!(!renderer.uses_terminal_owned_images());
        assert!(renderer_for(ProtocolType::Kitty).uses_terminal_owned_images());
    }

    #[test]
    fn protocol_override_has_safe_defaults_and_explicit_fallbacks() {
        assert_eq!(
            image_protocol_preference(None),
            ImageProtocolPreference::Auto
        );
        assert_eq!(
            image_protocol_preference(Some("unknown")),
            ImageProtocolPreference::Auto
        );
        assert_eq!(
            image_protocol_preference(Some("off")),
            ImageProtocolPreference::Off
        );
        assert_eq!(
            image_protocol_preference(Some("halfblocks")),
            ImageProtocolPreference::Force(ProtocolType::Halfblocks)
        );
        assert_eq!(
            image_protocol_preference(Some("kitty")),
            ImageProtocolPreference::Force(ProtocolType::Kitty)
        );
    }

    #[test]
    fn ghostty_auto_mode_prefers_stable_halfblocks() {
        assert_eq!(
            stable_auto_protocol(Some("ghostty")),
            Some(ProtocolType::Halfblocks)
        );
        assert_eq!(stable_auto_protocol(Some("iTerm.app")), None);
    }

    #[test]
    fn halfblock_renderer_draws_without_native_terminal_support() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pixel.png");
        image::DynamicImage::new_rgb8(4, 4)
            .save(&path)
            .expect("save fixture");

        let backend = ratatui::backend::TestBackend::new(8, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let mut renderer = renderer_for(ProtocolType::Halfblocks);
        terminal
            .draw(|frame| {
                assert!(renderer.render_path(frame, &path, Rect::new(0, 0, 4, 2)));
            })
            .expect("draw");

        assert_eq!(renderer.protocols.len(), 1);
    }
}
