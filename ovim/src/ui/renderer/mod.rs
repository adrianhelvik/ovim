// Public API re-exports
pub use cat_animation::CatAnimation;
pub use core::Renderer;
pub use dashboard::MENU_ITEMS;

// Internal modules
pub mod agent_tree;
pub mod ai_chat;
mod ai_chat_layout;
mod buffer;
pub mod cat_animation;
pub mod conversation_tree;
mod core;
pub mod dashboard;
mod debug_panels;
mod display_math;
mod file_tree_widget;
mod helpers;
mod layout;
pub mod line_cache;
pub mod lsp_manager;
mod markdown;
mod markdown_conceal;
mod overlays;
mod picker_widget;
mod status_widgets;
mod styles;
mod terminal_images;
