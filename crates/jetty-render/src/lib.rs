mod chrome;
mod colors;
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
mod preedit;
mod palette;
mod status;
mod ring;
mod ui_palette;
pub use ui_palette::{ensure_contrast, UiPalette};
mod grid_geom;
pub use chrome::{
    clip_head, clip_tail, fit_head, fit_tail, ChromeMeasure, ChromeMetrics, MonoMeasure,
    CHROME_ADVANCE, MAX_LABEL_CHARS, OVERLAY_SCALE, PILL_H_BASE, STATUS_H_BASE, UI_FONT_BASE,
};
pub use gpu::{AcquireError, GpuContext, GpuShared};
pub use text::{GridPaint, TextLayer};
pub use colors::{
    contrast_ratio, cursor_text_color, relative_luminance, selection_bg, selection_paint, SelectionPaint,
    SELECTION_MIN_CONTRAST,
};
pub use quad::{QuadLayer, Rect, cell_bg_rects, default_bg_clear, scrollbar_rect, scrollbar_rect_geom, scrollbar_offset_from_cursor, text_decoration_rects, link_underline_rects, failed_marker_rects, grid_decoration_key, cursor_rects, cursor_rects_split, SCROLLBAR_W};
pub use panel::{build_panel, EffectsParams, NotifyParams, PanelView, PanelGeom, PANEL_W, PANEL_H,
                EFFECTS_CONTENT_H, EFFECTS_VISIBLE_H, CHAR_W_FALLBACK};
pub use mask::{CornerMask, all_radii_flat, rounded_rect_coverage, rounded_rect_coverage_per};
pub use reveal::{BayerReveal, bayer4, reveal_coverage};
pub use phosphor::PhosphorIgnition;
pub use liquid::LiquidDrop;
pub use focus::FocusPull;
pub use crt::{Crt, CrtUniform, CRT_FLAG_ROLL, CRT_FLAG_FLICKER, CRT_FLAG_JITTER};
pub use image_layer::{ImageDraw, ImageLayer};
pub use caret_fx::{CaretFx, CaretFxUniform};
pub use menu::{build_context_menu, build_menu, ContextMenu, MENU_HINTS, MENU_ITEMS};
pub use help::{build_help_overlay, default_help_rows, HelpOverlay, HELP_ROWS};
pub use confirm::{build_confirm, build_confirm_close, ConfirmPopup};
pub use tabbar::{
    build_detached_bar, build_detached_bar_styled, build_tab_bar, build_tab_bar_ex, build_tab_bar_styled,
    detached_close_rect, detached_help_rect, tab_color_name, tab_color_rgb, valid_tab_color, CloseButton,
    CtrlHover, DetachedBar, TabActivity, TabBar, TabBarOpts, TabDeco, TabStyle, CONTROLS_W, STRIP_PAD,
    TABBAR_H, TAB_COLORS,
};
pub use welcome::{build_welcome_overlay, WelcomeOverlay};
pub use search_bar::{
    build_search_bar, search_current_fg, search_hit_rects, search_recolor_spans, SearchBar,
};
pub use hints::{build_copy_pill, build_hint_overlay, copy_cursor_rects, CopyPill, HintOverlay};
pub use preedit::{build_preedit_overlay, PreeditOverlay, MAX_PREEDIT_CHARS};
pub use palette::{build_command_palette, CommandPalette, PaletteRow, MAX_PALETTE_ROWS};
pub use status::{build_status_strip, build_toast_pill, StatusStrip, ToastPill};
pub use grid_geom::{failed_marker_x, grid_dims, padding_px, shift_labels_x, shift_x, GridOrigin, PADDING_MAX};
pub use text::{clamp_line_height, LINE_HEIGHT_DEFAULT, LINE_HEIGHT_MAX, LINE_HEIGHT_MIN};
pub use quad::{scrollbar_gutter_px, scrollbar_thumb_color, ScrollbarTrack};
pub use ring::{ring_coverage, ring_width_px, FocusRing, RingUniform};
