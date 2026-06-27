//! Pure tab-naming logic. No zellij-tile dependency.

/// Display-relevant subset of a terminal pane (from PaneInfo).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneLite {
    pub id: u32,
    pub title: String,
    pub is_focused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_lite_defaults_are_empty() {
        let p = PaneLite::default();
        assert_eq!(p.id, 0);
        assert!(p.title.is_empty());
        assert!(!p.is_focused);
    }
}
