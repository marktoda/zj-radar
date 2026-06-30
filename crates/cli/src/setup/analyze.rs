use super::*;

use toml_edit::{DocumentMut, Item};

/// Raw, already-read environment for Zellij setup. The ONLY layer that touched
/// the filesystem — `analyze_zellij` is pure over this struct.
pub(crate) struct ZellijEnv {
    pub config_text:           Option<String>,
    pub layout_text:           Option<String>,
    pub permissions_text:      Option<String>,
    pub codex_hooks_text:      Option<String>,
    pub installed_plugins_text: Option<String>,
    pub wasm_present:          bool,
    pub config_managed:        bool,
    pub wasm_path:             String,
}

/// Every derived fact about the current Zellij setup state, in one place. Both
/// `check` (renders) and `install` (gates) read these; the derivation is here so
/// "is our alias present?" has exactly one definition.
pub(crate) struct ZellijFacts {
    pub managed_alias_present:   bool,
    pub unmanaged_alias_present: bool,
    pub alias_is_store_path:     bool,
    pub wasm_present:            bool,
    pub has_rail:                Option<bool>,
    pub granted:                 Option<bool>,
    pub producer_wired:          bool,
    pub config_managed:          bool,
}

/// Pure: derive every Zellij setup fact from already-read inputs. No I/O.
pub(crate) fn analyze_zellij(env: &ZellijEnv) -> ZellijFacts {
    let lines: Vec<String> = env.config_text.as_deref().map(split_lines).unwrap_or_default();
    let managed_alias_present = lines.iter().any(|l| l.trim() == ZELLIJ_ALIAS_BEGIN);
    let mut lines_without_managed = lines.clone();
    strip_managed_zellij_alias(&mut lines_without_managed);
    let unmanaged_alias_present = has_unmanaged_radar_alias(&lines_without_managed);
    let alias_is_store_path =
        env.config_text.as_deref().is_some_and(|t| t.contains("/nix/store/"));
    let has_rail = env.layout_text.as_deref().map(|t| crate::layout::analyze(t).has_rail);
    let granted = env
        .permissions_text
        .as_deref()
        .map(|t| crate::run::wasm_is_granted(t, &env.wasm_path));
    let claude_present = crate::run::claude_producer_wired(env.installed_plugins_text.as_deref());
    let producer_wired =
        crate::run::producer_hint(env.codex_hooks_text.as_deref(), claude_present).is_none();
    ZellijFacts {
        managed_alias_present,
        unmanaged_alias_present,
        alias_is_store_path,
        wasm_present: env.wasm_present,
        has_rail,
        granted,
        producer_wired,
        config_managed: env.config_managed,
    }
}

/// Derived state of Codex's `[features].hooks` switch.
pub(crate) enum CodexHooksFeature {
    EnabledOrUnset,
    Disabled,
    ConfigError(String),
}

/// Derived state of the legacy `notify` slot in Codex `config.toml`.
pub(crate) enum CodexNotifyState {
    ConfigAbsent,
    NotInstalled,
    Ours,
    Foreign,
    ConfigError(String),
}

/// Raw, already-read environment for Codex setup. The only IO layer.
pub(crate) struct CodexEnv {
    pub codex_on_path:    bool,
    pub zj_radar_on_path: bool,
    pub config_text:      Option<String>,
    pub hooks_text:       Option<String>,
}

/// Every derived fact about Codex setup state. The legacy-vs-hooks choice is a
/// flag the consumer projects on — NOT a fact — so both surfaces are observed.
pub(crate) struct CodexFacts {
    pub codex_on_path:     bool,
    pub zj_radar_on_path:  bool,
    pub hooks_feature:     CodexHooksFeature,
    pub notify:            CodexNotifyState,
    /// `None` = hooks.json absent; `Some(Ok(n))` = n marker-owned events; `Some(Err)` = parse error.
    pub owned_hook_events: Option<Result<usize, String>>,
}

/// Pure: derive every Codex setup fact from already-read inputs. No I/O.
pub(crate) fn analyze_codex(env: &CodexEnv) -> CodexFacts {
    let hooks_feature = match env.config_text.as_deref().map(codex_hooks_disabled_in_config) {
        Some(Ok(true)) => CodexHooksFeature::Disabled,
        Some(Ok(false)) | None => CodexHooksFeature::EnabledOrUnset,
        Some(Err(e)) => CodexHooksFeature::ConfigError(e),
    };
    let notify = match env.config_text.as_deref() {
        None => CodexNotifyState::ConfigAbsent,
        Some(text) => match text.parse::<DocumentMut>() {
            Ok(doc) if notify_is_ours(doc.get("notify")) => CodexNotifyState::Ours,
            Ok(doc) if doc.get("notify").is_some() => CodexNotifyState::Foreign,
            Ok(_) => CodexNotifyState::NotInstalled,
            Err(e) => CodexNotifyState::ConfigError(e.to_string()),
        },
    };
    let owned_hook_events = env.hooks_text.as_deref().map(codex_owned_hook_event_count);
    CodexFacts {
        codex_on_path: env.codex_on_path,
        zj_radar_on_path: env.zj_radar_on_path,
        hooks_feature,
        notify,
        owned_hook_events,
    }
}

fn codex_owned_hook_event_count(existing: &str) -> Result<usize, String> {
    let file = parse_hooks_file(existing)?;
    Ok(CODEX_HOOK_EVENTS
        .iter()
        .filter(|event| {
            file.hooks.get(**event).is_some_and(|groups| {
                groups
                    .iter()
                    .filter_map(|group| group.hooks.as_ref())
                    .flatten()
                    .any(codex_hook_handler_is_ours)
            })
        })
        .count())
}

fn codex_hooks_disabled_in_config(existing: &str) -> Result<bool, String> {
    let doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| format!("config.toml is not valid TOML: {e}"))?;
    Ok(doc
        .get("features")
        .and_then(Item::as_table_like)
        .and_then(|features| {
            features
                .get("hooks")
                .or_else(|| features.get("codex_hooks"))
                .and_then(Item::as_bool)
        })
        == Some(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_zellij_derives_managed_and_unmanaged_alias_separately() {
        // Managed alias block present, no unmanaged line.
        let managed = format!("plugins {{\n{ZELLIJ_ALIAS_BEGIN}\n    radar location=\"file:/x.wasm\"\n{ZELLIJ_ALIAS_END}\n}}\n");
        let env = ZellijEnv {
            config_text: Some(managed),
            layout_text: None,
            permissions_text: None,
            codex_hooks_text: None,
            installed_plugins_text: None,
            wasm_present: false,
            config_managed: false,
            wasm_path: "/x.wasm".to_string(),
        };
        let f = analyze_zellij(&env);
        assert!(f.managed_alias_present, "managed marker must be detected");
        assert!(!f.unmanaged_alias_present, "no unmanaged alias here");
    }

    #[test]
    fn analyze_zellij_derives_has_rail_and_grant_from_text() {
        let wasm_path = "/home/user/.config/zellij/plugins/zj_radar.wasm";
        let layout = "layout {\n    plugin location=\"radar\"\n}\n";
        let perms = format!("\"{wasm_path}\" {{\n    ReadApplicationState\n}}\n");
        let env = ZellijEnv {
            config_text: None,
            layout_text: Some(layout.to_string()),
            permissions_text: Some(perms),
            codex_hooks_text: None,
            installed_plugins_text: None,
            wasm_present: true,
            config_managed: false,
            wasm_path: wasm_path.to_string(),
        };
        let f = analyze_zellij(&env);
        assert_eq!(f.has_rail, Some(true), "layout text with radar plugin has rail");
        assert_eq!(f.granted, Some(true), "permissions naming the wasm path is granted");
        assert!(f.wasm_present);
    }

    #[test]
    fn analyze_zellij_absent_files_are_none_not_false() {
        let env = ZellijEnv {
            config_text: None,
            layout_text: None,
            permissions_text: None,
            codex_hooks_text: None,
            installed_plugins_text: None,
            wasm_present: false,
            config_managed: false,
            wasm_path: "/x.wasm".to_string(),
        };
        let f = analyze_zellij(&env);
        assert_eq!(f.has_rail, None, "no layout file -> None, distinct from Some(false)");
        assert_eq!(f.granted, None, "no permissions file -> None");
        assert!(!f.producer_wired, "no codex hooks and no claude plugin -> not wired");
    }

    #[test]
    fn analyze_codex_classifies_notify_states() {
        let ours = "notify = [\"zj-radar\", \"notify\", \"codex\"]\n";
        let foreign = "notify = [\"other\"]\n";
        let mk = |cfg: Option<&str>| analyze_codex(&CodexEnv {
            codex_on_path: true,
            zj_radar_on_path: true,
            config_text: cfg.map(str::to_string),
            hooks_text: None,
        });
        assert!(matches!(mk(Some(ours)).notify, CodexNotifyState::Ours));
        assert!(matches!(mk(Some(foreign)).notify, CodexNotifyState::Foreign));
        assert!(matches!(mk(Some("a = 1\n")).notify, CodexNotifyState::NotInstalled));
        assert!(matches!(mk(None).notify, CodexNotifyState::ConfigAbsent));
    }

    #[test]
    fn analyze_codex_hooks_feature_and_event_count() {
        let cfg_disabled = "[features]\nhooks = false\n";
        let f = analyze_codex(&CodexEnv {
            codex_on_path: true,
            zj_radar_on_path: true,
            config_text: Some(cfg_disabled.to_string()),
            hooks_text: None,
        });
        assert!(matches!(f.hooks_feature, CodexHooksFeature::Disabled));
        assert!(f.owned_hook_events.is_none(), "no hooks.json -> None");
    }
}
