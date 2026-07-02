use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CheckLevel {
    Ok,
    Warn,
    Missing,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CheckItem {
    level: CheckLevel,
    name: &'static str,
    detail: String,
}

impl CheckItem {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Ok,
            name,
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Warn,
            name,
            detail: detail.into(),
        }
    }

    fn missing(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Missing,
            name,
            detail: detail.into(),
        }
    }
}

pub(crate) fn check_codex(legacy_notify: bool) {
    let env = CodexEnv {
        codex_on_path:    which("codex"),
        zj_radar_on_path: which("zj-radar"),
        config_text:      codex_config_path().and_then(|p| std::fs::read_to_string(p).ok()),
        hooks_text:       codex_hooks_path().and_then(|p| std::fs::read_to_string(p).ok()),
    };
    let items = codex_check_items(&analyze_codex(&env), legacy_notify);
    println!("codex:");
    print_check_items(&items);
}

/// `CheckItem`s for `zj-radar setup zellij --check`. Pure over fully-derived
/// `ZellijFacts`; the derivation lives in `analyze_zellij`.
pub(crate) fn zellij_check_items(f: &ZellijFacts) -> Vec<CheckItem> {
    let mut items = Vec::new();

    // 1. alias — "present" means managed marker OR an unmanaged alias line.
    let alias_present = f.managed_alias_present || f.unmanaged_alias_present;
    items.push(match (alias_present, f.alias_is_store_path) {
        (false, _) => CheckItem::missing("alias", "radar plugin alias not found in config.kdl"),
        (true, true) => CheckItem::warn(
            "alias",
            "alias points at /nix/store/ path — grant won't persist across rebuilds; run `setup zellij` after each rebuild",
        ),
        (true, false) => CheckItem::ok("alias", "radar plugin alias present in config.kdl"),
    });

    // 2. wasm
    items.push(if f.wasm_present {
        CheckItem::ok("wasm", "wasm plugin file present")
    } else {
        CheckItem::missing(
            "wasm",
            "wasm plugin file not found — run `zj-radar setup zellij --wasm <path>` or `--download`",
        )
    });

    // 3. layout (rail)
    items.push(match f.has_rail {
        None => CheckItem::warn("layout", "no default layout found"),
        Some(true) => CheckItem::ok("layout", "default layout has the radar rail"),
        Some(false) => CheckItem::missing(
            "layout",
            "default layout does not have the radar rail — run `zj-radar setup zellij` or paste the snippet",
        ),
    });

    // 4. grant
    items.push(match f.granted {
        None => CheckItem::warn("grant", "no permissions.kdl found"),
        Some(true) => CheckItem::ok("grant", "wasm is granted in permissions.kdl"),
        Some(false) => CheckItem::missing("grant", "wasm not granted — run `zj-radar setup zellij --grant`"),
    });

    // 5. producer
    items.push(if f.producer_wired {
        CheckItem::ok("producer", "a producer is wired (Codex hooks or Claude plugin)")
    } else {
        CheckItem::missing(
            "producer",
            "no producer detected — run `zj-radar setup codex` or enable the Claude plugin",
        )
    });

    // 6. managed config (only emit when true)
    if f.config_managed {
        items.push(CheckItem::warn(
            "managed config",
            "config.kdl is managed (symlink); edits may be overwritten",
        ));
    }

    items
}

pub(crate) fn check_zellij(layout_name: Option<&str>) {
    let config_dir = zellij_config_dir();
    let config_path = zellij_config_path(&config_dir);
    let wasm_dest = zellij_wasm_dest(&config_dir);
    let config_text = std::fs::read_to_string(&config_path).ok();
    // Same resolution as the install path: --layout, else the config's
    // `default_layout`, else `default` — so the doctor inspects the layout
    // Zellij actually loads (and the one a `--layout` install just wrote).
    let layout_path = config_dir.join("layouts").join(format!(
        "{}.kdl",
        crate::setup::detect::resolve_layout_name(layout_name, config_text.as_deref())
    ));
    let env = ZellijEnv {
        config_text,
        layout_text: std::fs::read_to_string(&layout_path).ok(),
        permissions_text: crate::run::zellij_permissions_path()
            .and_then(|p| std::fs::read_to_string(p).ok()),
        codex_hooks_text: super::codex_hooks_text(),
        installed_plugins_text: dirs::home_dir()
            .and_then(|h| std::fs::read_to_string(h.join(".claude/plugins/installed_plugins.json")).ok()),
        wasm_present: wasm_dest.is_file(),
        config_managed: config_is_managed(&config_path),
        wasm_path: wasm_dest.to_string_lossy().into_owned(),
    };
    let items = zellij_check_items(&analyze_zellij(&env));
    println!("zellij:");
    print_check_items(&items);
}

fn print_check_items(items: &[CheckItem]) {
    for item in items {
        let status = match item.level {
            CheckLevel::Ok => "ok",
            CheckLevel::Warn => "warn",
            CheckLevel::Missing => "missing",
        };
        println!("  {status} {}: {}", item.name, item.detail);
    }
}

pub(crate) fn codex_check_items(f: &CodexFacts, legacy_notify: bool) -> Vec<CheckItem> {
    let mut items = Vec::new();
    items.push(if f.codex_on_path {
        CheckItem::ok("codex binary", "found on PATH")
    } else {
        CheckItem::missing("codex binary", "not found on PATH")
    });
    items.push(if f.zj_radar_on_path {
        CheckItem::ok("zj-radar binary", "found on PATH")
    } else {
        CheckItem::missing("zj-radar binary", "not found on PATH")
    });

    items.push(match &f.hooks_feature {
        CodexHooksFeature::Disabled => {
            CheckItem::warn("hooks feature", "`[features].hooks = false` disables Codex hooks")
        }
        CodexHooksFeature::EnabledOrUnset => {
            CheckItem::ok("hooks feature", "enabled or unset in config.toml")
        }
        CodexHooksFeature::ConfigError(e) => CheckItem::warn("config.toml", e.clone()),
    });

    if legacy_notify {
        items.push(match &f.notify {
            CodexNotifyState::ConfigAbsent => {
                CheckItem::missing("legacy notify", "config.toml not found")
            }
            CodexNotifyState::Ours => CheckItem::ok("legacy notify", "zj-radar owns Codex notify"),
            CodexNotifyState::Foreign => {
                CheckItem::warn("legacy notify", "another command owns Codex notify")
            }
            CodexNotifyState::NotInstalled => {
                CheckItem::missing("legacy notify", "Codex notify is not installed")
            }
            CodexNotifyState::ConfigError(e) => CheckItem::warn(
                "config.toml",
                format!("config.toml is not valid TOML: {e}"),
            ),
        });
    } else {
        items.push(match &f.owned_hook_events {
            None => CheckItem::missing("hooks.json", "zj-radar Codex hooks are not installed"),
            Some(Ok(count)) if *count == CODEX_HOOK_EVENTS.len() => {
                CheckItem::ok("hooks.json", "all zj-radar Codex hooks installed")
            }
            Some(Ok(count)) if *count > 0 => CheckItem::warn(
                "hooks.json",
                format!("partial zj-radar hook install ({count}/{})", CODEX_HOOK_EVENTS.len()),
            ),
            Some(Ok(_)) => {
                CheckItem::missing("hooks.json", "zj-radar Codex hooks are not installed")
            }
            Some(Err(e)) => CheckItem::warn("hooks.json", e.clone()),
        });
        if matches!(f.notify, CodexNotifyState::Foreign) {
            items.push(CheckItem::ok(
                "legacy notify",
                "foreign notify is preserved; hooks do not use the notify slot",
            ));
        }
    }

    if !legacy_notify {
        items.push(CheckItem::warn(
            "hook trust",
            "run `/hooks` in Codex after install or hook changes",
        ));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_check_reports_hook_setup_ready_with_trust_reminder() {
        let hooks = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("model = \"x\"\n".to_string()),
            hooks_text:       Some(hooks.to_string()),
        });
        let items = codex_check_items(&facts, false);
        assert!(items.contains(&CheckItem::ok("codex binary", "found on PATH")));
        assert!(items.contains(&CheckItem::ok("zj-radar binary", "found on PATH")));
        assert!(items.contains(&CheckItem::ok(
            "hooks feature",
            "enabled or unset in config.toml"
        )));
        assert!(items.contains(&CheckItem::ok(
            "hooks.json",
            "all zj-radar Codex hooks installed"
        )));
        assert!(items.iter().any(|item| item.name == "hook trust"
            && item.level == CheckLevel::Warn
            && item.detail.contains("/hooks")));
    }

    #[test]
    fn codex_check_warns_when_hooks_feature_is_disabled() {
        let hooks = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("[features]\nhooks = false\n".to_string()),
            hooks_text:       Some(hooks.to_string()),
        });
        let items = codex_check_items(&facts, false);
        assert!(items.iter().any(|item| item.name == "hooks feature"
            && item.level == CheckLevel::Warn
            && item.detail.contains("hooks = false")));
    }

    #[test]
    fn codex_check_reports_partial_or_malformed_hooks() {
        let partial = r#"{
          "hooks": {
            "Stop": [
              {
                "hooks": [
                  {
                    "type": "command",
                    "command": "ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex"
                  }
                ]
              }
            ]
          }
        }"#;
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      None,
            hooks_text:       Some(partial.to_string()),
        });
        let items = codex_check_items(&facts, false);
        assert!(items.iter().any(|item| item.name == "hooks.json"
            && item.level == CheckLevel::Warn
            && item.detail.contains("partial")));

        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      None,
            hooks_text:       Some("not json".to_string()),
        });
        let items = codex_check_items(&facts, false);
        assert!(items.iter().any(|item| item.name == "hooks.json"
            && item.level == CheckLevel::Warn
            && item.detail.contains("not valid JSON")));
    }

    #[test]
    fn codex_check_notes_foreign_notify_is_preserved_for_hooks() {
        let hooks = match edit_codex_hooks("", true).unwrap() {
            Outcome::Changed(s) => s,
            o => panic!("{o:?}"),
        };
        let config = "notify = [\"/other\", \"turn-ended\"]\n";
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some(config.to_string()),
            hooks_text:       Some(hooks.to_string()),
        });
        let items = codex_check_items(&facts, false);
        assert!(items.iter().any(|item| item.name == "legacy notify"
            && item.level == CheckLevel::Ok
            && item.detail.contains("preserved")));
    }

    #[test]
    fn codex_check_legacy_notify_mode_reports_notify_slot() {
        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("notify = [\"zj-radar\", \"notify\", \"codex\"]\n".to_string()),
            hooks_text:       None,
        });
        let items = codex_check_items(&facts, true);
        assert!(items.contains(&CheckItem::ok(
            "legacy notify",
            "zj-radar owns Codex notify"
        )));

        let facts = analyze_codex(&CodexEnv {
            codex_on_path:    true,
            zj_radar_on_path: true,
            config_text:      Some("notify = [\"/other\"]\n".to_string()),
            hooks_text:       None,
        });
        let items = codex_check_items(&facts, true);
        assert!(items.iter().any(|item| item.name == "legacy notify"
            && item.level == CheckLevel::Warn
            && item.detail.contains("another command")));
        assert!(
            !items.iter().any(|item| item.name == "hook trust"),
            "legacy notify mode should not ask users to trust hooks"
        );
    }

    /// Helper: all-good `ZellijFacts` so tests override only the dimension they
    /// care about. (The raw-text→fact derivation is tested in `analyze_zellij_*`.)
    fn all_good_facts() -> ZellijFacts {
        ZellijFacts {
            managed_alias_present:   false,
            unmanaged_alias_present: true,
            alias_is_store_path:     false,
            wasm_present:            true,
            has_rail:                Some(true),
            granted:                 Some(true),
            producer_wired:          true,
            config_managed:          false,
        }
    }

    fn all_good_check_items() -> Vec<CheckItem> {
        zellij_check_items(&all_good_facts())
    }

    #[test]
    fn zellij_check_items_all_ok() {
        let items = all_good_check_items();
        assert!(items.iter().any(|i| i.name == "alias" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "wasm" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "layout" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "grant" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "producer" && i.level == CheckLevel::Ok));
        // managed config not emitted when false
        assert!(!items.iter().any(|i| i.name == "managed config"));
    }

    #[test]
    fn zellij_check_items_nix_store_alias_warns() {
        let mut f = all_good_facts();
        f.alias_is_store_path = true;
        let items = zellij_check_items(&f);
        let alias = items.iter().find(|i| i.name == "alias").expect("alias item");
        assert_eq!(alias.level, CheckLevel::Warn, "nix-store alias must warn");
        assert!(alias.detail.contains("nix/store"), "warn detail must mention nix/store");
        assert!(alias.detail.contains("rebuild"), "warn detail must mention rebuild");
    }

    #[test]
    fn zellij_check_items_rail_less_layout_is_missing() {
        let mut f = all_good_facts();
        f.has_rail = Some(false);
        let items = zellij_check_items(&f);
        let layout_item = items.iter().find(|i| i.name == "layout").expect("layout item");
        assert_eq!(layout_item.level, CheckLevel::Missing, "layout without rail must be missing");
        assert!(layout_item.detail.contains("setup zellij"), "hint must mention setup zellij");
    }

    #[test]
    fn zellij_check_items_ungranted_wasm_is_missing() {
        let mut f = all_good_facts();
        f.granted = Some(false);
        let items = zellij_check_items(&f);
        let grant = items.iter().find(|i| i.name == "grant").expect("grant item");
        assert_eq!(grant.level, CheckLevel::Missing, "ungranted wasm must be missing");
        assert!(grant.detail.contains("--grant"), "hint must mention --grant");
    }

    #[test]
    fn zellij_check_items_managed_config_warns() {
        let mut f = all_good_facts();
        f.config_managed = true;
        let items = zellij_check_items(&f);
        let managed = items.iter().find(|i| i.name == "managed config").expect("managed config item");
        assert_eq!(managed.level, CheckLevel::Warn, "managed config must warn");
        assert!(managed.detail.contains("symlink"), "warn detail must mention symlink");
    }

    #[test]
    fn zellij_check_items_missing_alias_is_missing() {
        let mut f = all_good_facts();
        f.unmanaged_alias_present = false;
        let items = zellij_check_items(&f);
        let alias = items.iter().find(|i| i.name == "alias").expect("alias item");
        assert_eq!(alias.level, CheckLevel::Missing);
    }

    #[test]
    fn zellij_check_items_no_layout_warns() {
        let mut f = all_good_facts();
        f.has_rail = None;
        let items = zellij_check_items(&f);
        let layout_item = items.iter().find(|i| i.name == "layout").expect("layout item");
        assert_eq!(layout_item.level, CheckLevel::Warn, "missing layout file should warn");
    }

    #[test]
    fn zellij_check_items_no_permissions_warns() {
        let mut f = all_good_facts();
        f.granted = None;
        let items = zellij_check_items(&f);
        let grant = items.iter().find(|i| i.name == "grant").expect("grant item");
        assert_eq!(grant.level, CheckLevel::Warn, "no permissions.kdl should warn");
    }

    #[test]
    fn zellij_check_items_no_producer_is_missing() {
        let mut f = all_good_facts();
        f.producer_wired = false;
        let items = zellij_check_items(&f);
        let producer = items.iter().find(|i| i.name == "producer").expect("producer item");
        assert_eq!(producer.level, CheckLevel::Missing);
        assert!(producer.detail.contains("setup codex"), "hint must mention setup codex");
    }

    #[test]
    fn zellij_check_items_order_is_stable() {
        let items = all_good_check_items();
        let names: Vec<&str> = items.iter().map(|i| i.name).collect();
        assert_eq!(names, &["alias", "wasm", "layout", "grant", "producer"]);
    }
}
