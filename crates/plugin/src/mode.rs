//! The Zellij input mode, as the `show_mode` footer label sees it.
//!
//! A host-testable mirror of `zellij_tile::prelude::InputMode` (wasm-only, so
//! host tests can't name it); `lib.rs` maps `ModeInfo.mode` into it on every
//! `Event::ModeUpdate`. Zellij sends that event to the plugins in a client's
//! active tab on every mode change and on every tab switch, so the visible
//! rail always holds the current mode. Two clients in different modes viewing
//! the same tab share one rail: the last `ModeUpdate` wins.

/// One variant per `InputMode` variant, so the mapping in `lib.rs` stays an
/// exhaustive match: a new Zellij mode fails the wasm build instead of
/// silently rendering as something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Locked,
    Resize,
    Pane,
    Tab,
    Scroll,
    EnterSearch,
    Search,
    RenameTab,
    RenamePane,
    Session,
    Move,
    Prompt,
    Tmux,
}

impl Mode {
    /// The footer label. Sub-modes the user experiences as one step share a
    /// label: typing a needle and stepping through matches are both `SEARCH`,
    /// renaming a tab or a pane is `RENAME`.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Locked => "LOCKED",
            Mode::Resize => "RESIZE",
            Mode::Pane => "PANE",
            Mode::Tab => "TAB",
            Mode::Scroll => "SCROLL",
            Mode::EnterSearch | Mode::Search => "SEARCH",
            Mode::RenameTab | Mode::RenamePane => "RENAME",
            Mode::Session => "SESSION",
            Mode::Move => "MOVE",
            Mode::Prompt => "PROMPT",
            Mode::Tmux => "TMUX",
        }
    }

    /// `Normal` is the resting state, so the footer mutes it; every other
    /// mode is a state the user entered on purpose and gets the accent.
    pub fn is_normal(self) -> bool {
        self == Mode::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_merge_the_search_and_rename_sub_modes() {
        assert_eq!(Mode::EnterSearch.label(), Mode::Search.label());
        assert_eq!(Mode::Search.label(), "SEARCH");
        assert_eq!(Mode::RenameTab.label(), "RENAME");
        assert_eq!(Mode::RenamePane.label(), "RENAME");
        assert_eq!(Mode::Normal.label(), "NORMAL");
        assert_eq!(Mode::Tmux.label(), "TMUX");
    }

    #[test]
    fn only_normal_is_normal() {
        assert!(Mode::Normal.is_normal());
        assert!(!Mode::Locked.is_normal());
    }
}
