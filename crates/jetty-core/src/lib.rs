pub mod fuzzy;
mod pty;
mod snapshot;
mod terminal;
pub mod theme;
pub mod url;

pub use fuzzy::{fuzzy_match, FuzzyMatch};
pub use pty::{set_advertised_version, PtySession};
pub use snapshot::{attr, CellSnapshot, CursorShapeSnap, GridSnapshot, SearchHit, SHAPE_MASK};
pub use terminal::{
    CommandCompletion, LinkHit, Terminal, OSC52_MAX_BYTES, SEARCH_MAX_MATCHES, SEARCH_MAX_QUERY,
};
pub use theme::Theme;
pub use theme::{builtins, set_registry, theme_at, theme_count, theme_index, theme_list};
