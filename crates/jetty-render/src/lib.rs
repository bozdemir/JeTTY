mod gpu;
mod text;
mod quad;
mod panel;
mod menu;
mod help;
mod confirm;
mod tabbar;
mod mask;
mod reveal;
mod phosphor;
mod liquid;
mod focus;
mod crt;
mod image_layer;
mod welcome;
mod caret_fx;
mod search_bar;
mod hints;
mod palette;
pub use gpu::GpuContext;
pub use text::TextLayer;
pub use quad::{QuadLayer, Rect, cell_bg_rects, default_bg_clear, scrollbar_rect, scrollbar_rect_geom, scrollbar_offset_from_cursor, text_decoration_rects, link_underline_rects, failed_marker_rects, grid_decoration_key, cursor_rects, SCROLLBAR_W};
pub use panel::{build_panel, EffectsParams, NotifyParams, PanelView, PanelGeom, PANEL_W, PANEL_H,
                EFFECTS_CONTENT_H, EFFECTS_VISIBLE_H, CHAR_W_FALLBACK};
pub use mask::{CornerMask, rounded_rect_coverage, rounded_rect_coverage_per};
pub use reveal::{BayerReveal, bayer4, reveal_coverage};
pub use phosphor::PhosphorIgnition;
pub use liquid::LiquidDrop;
pub use focus::FocusPull;
pub use crt::{Crt, CrtUniform, CRT_FLAG_ROLL, CRT_FLAG_FLICKER, CRT_FLAG_JITTER};
pub use image_layer::{ImageDraw, ImageLayer};
pub use caret_fx::{CaretFx, CaretFxUniform};
pub use menu::{build_context_menu, build_menu, ContextMenu};
pub use help::{build_help_overlay, default_help_rows, HelpOverlay, HELP_ROWS};
pub use confirm::{build_confirm, build_confirm_close, ConfirmPopup};
pub use tabbar::{
    build_detached_bar, build_tab_bar, build_tab_bar_ex, detached_close_rect, CtrlHover,
    DetachedBar, TabActivity, TabBar, CONTROLS_W, STRIP_PAD, TABBAR_H,
};
pub use welcome::{build_welcome_overlay, WelcomeOverlay};
pub use search_bar::{build_search_bar, search_hit_rects, SearchBar};
pub use hints::{build_copy_pill, build_hint_overlay, copy_cursor_rects, CopyPill, HintOverlay};
pub use palette::{build_command_palette, CommandPalette, PaletteRow, MAX_PALETTE_ROWS};
