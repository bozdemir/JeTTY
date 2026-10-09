mod chrome;
mod colors;
mod cursor;
mod cursor_trail;
mod gpu;
mod text;
mod builtin;
mod emoji;
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
mod transform;
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
mod backdrop;
pub mod backdrop_image;
pub use chrome::{
    clip_head, clip_tail, fit_head, fit_tail, ChromeMeasure, ChromeMetrics, MonoMeasure,
    CHROME_ADVANCE, MAX_LABEL_CHARS, OVERLAY_SCALE, PILL_H_BASE, STATUS_H_BASE, UI_FONT_BASE,
};
pub use gpu::{AcquireError, GpuContext, GpuShared};
pub use text::{GridPaint, TextLayer};
pub use colors::{
    caret_flash_target, contrast_ratio, cursor_text_color, is_light_bg, relative_luminance, selection_bg,
    selection_paint, SelectionPaint, CARET_FLASH_MIN_CONTRAST, SELECTION_MIN_CONTRAST,
};
pub use cursor_trail::{
    rect_corners, simulate_trail, CursorTrailLayer, TrailFrame, TrailModel, TrailParams, TrailPos, TrailUniform,
    TRAIL_DWELL,
};
pub use cursor::{
    caret_flash_color, cursor_colors, cursor_draw, cursor_guide_rect, cursor_trail_rect, CursorColor, CursorColors, CursorDraw, CursorStyle,
    UnderlineCursor, UnfocusedCursor, CURSOR_THICKNESS_DEFAULT, CURSOR_THICKNESS_MAX, CURSOR_THICKNESS_MIN,
};
pub use quad::{QuadLayer, Rect, cell_bg_rects, default_bg_clear, scrollbar_rect, scrollbar_rect_geom, scrollbar_offset_from_cursor, text_decoration_rects, link_underline_rects, failed_marker_rects, grid_decoration_key, SCROLLBAR_W};
pub use quad::{link_underline_rects_at, text_decoration_rects_at, Deco, UnderlineGeom};
pub use panel::{
    build_panel, gallery_order, is_user_theme, theme_is_light, track_knob, CtlId, CtlPart, CtlRow, CtlShow, Label,
    PanelGeom, PanelHit, PanelInput, PanelItem, PanelView, ResetState, RowState, ThemeFilter, CHAR_W_FALLBACK,
    GALLERY_COLS, N_TABS, PANEL_H, PANEL_W, TAB_NAMES,
};
pub use mask::{CornerMask, all_radii_flat, rounded_rect_coverage, rounded_rect_coverage_per, rounded_rect_coverage_slid};
pub use reveal::{BayerReveal, bayer4, reveal_coverage};
pub use phosphor::PhosphorIgnition;
pub use liquid::LiquidDrop;
pub use focus::FocusPull;
pub use crt::{
    anim_seed, bloom_blur, crt_bloom_shader_source, crt_shader_source, srgb_to_linear, Crt, CrtExtUniform,
    CrtFrame, CrtKey, CrtParams, CrtSettings, CrtUniform, Phosphor, ANIM_SEED_FPS, BLOOM_STEP_MAX,
    CRT_FLAG_FLICKER, CRT_FLAG_JITTER, CRT_FLAG_ROLL,
};
pub use transform::{transform_params, transform_secs, SummonTransform, TransformKind};
pub use image_layer::{ImageDraw, ImageLayer};
pub use caret_fx::{caret_glow_look, caret_glow_scissor, CaretFx, CaretFxUniform};
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
pub use hints::{build_copy_pill, build_hint_overlay, copy_cursor_rects, CopyPill, HintOverlay, PillAvoid};
pub use preedit::{build_preedit_overlay, PreeditOverlay, MAX_PREEDIT_CHARS};
pub use palette::{build_command_palette, CommandPalette, PaletteRow, MAX_PALETTE_ROWS};
pub use status::{build_status_strip, build_toast_pill, StatusStrip, ToastPill};
pub use grid_geom::{failed_marker_x, grid_dims, padding_px, shift_labels_x, shift_x, GridOrigin, PADDING_MAX};
pub use text::{clamp_line_height, LINE_HEIGHT_DEFAULT, LINE_HEIGHT_MAX, LINE_HEIGHT_MIN};
pub use quad::{scrollbar_gutter_px, scrollbar_thumb_color, ScrollbarTrack};
pub use ring::{ring_coverage, ring_width_px, FocusRing, RingUniform};
pub use backdrop::{
    build_uniform as backdrop_uniform, curated_theme_ids, fit_transform, parallax_offset, parse_hex_color,
    readability_bounds, readable_ratio, resolve_look, smart_dim,
    theme_look, Backdrop, BackdropFit, BackdropFrame, BackdropMode, BackdropPattern, BackdropSettings, BackdropShape,
    BackdropUniform, GpuImage, ThemeLook,
};
