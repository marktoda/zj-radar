use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CheckLevel {
    Ok,
    Warn,
    Missing,
    /// Advice about state the doctor cannot observe (e.g. Codex hook trust).
    /// Prints as `note`; never counts toward the Missing exit code, and unlike
    /// `Warn` doesn't imply anything is off — a healthy install may carry one.
    Note,
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

    fn note(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Note,
            name,
            detail: detail.into(),
        }
    }
}

/// Returns true when any item is `Missing` — the doctor's contribution to the
/// process exit code, so `zj-radar setup --check && zj-radar run` can gate.
/// Warns don't fail: they're advice, not a broken install.
pub(crate) fn check_codex(legacy_notify: bool) -> bool {
    let env = CodexEnv {
        codex_on_path:    which("codex"),
        zj_radar_on_path: which("zj-radar"),
        config_text:      codex_config_path().and_then(|p| std::fs::read_to_string(p).ok()),
        hooks_text:       codex_hooks_path().and_then(|p| std::fs::read_to_string(p).ok()),
    };
    let items = codex_check_items(&analyze_codex(&env), legacy_notify);
    println!("codex:");
    print_check_items(&items)
}

/// `CheckItem`s for `zj-radar setup zellij --check`. Pure over fully-derived
/// `ZellijFacts` plus the NAME of the layout those facts were read for
/// (`--layout NAME`, else the config's `default_layout`, else `default`);
/// the derivation lives in `analyze_zellij` / `read_zellij_env`.
pub(crate) fn zellij_check_items(f: &ZellijFacts, layout_name: &str) -> Vec<CheckItem> {
    let mut items = Vec::new();

    // 0. the zellij binary itself — every other item is moot without it, and
    // an all-ok report on a zellij-less machine would be a --check lie.
    let floor = min_supported_zellij_display();
    items.push(match &f.zellij_version {
        None => CheckItem::missing("zellij binary", zellij_missing_message()),
        Some(v) if !zellij_version_is_supported(v) => CheckItem::warn(
            "zellij binary",
            format!(
                "found `{v}` — the sidebar needs Zellij {floor}+ (newer is fine; older \
                 versions predate the plugin API this build targets, and 0.44 patches \
                 before .3 let the sidebar pop out during layout swaps)"
            ),
        ),
        Some(v) => CheckItem::ok("zellij binary", format!("found on PATH ({v})")),
    });

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

    // 3. layout (rail) — name the layout actually inspected: "default layout"
    // was a lie under `--check --layout mine`.
    items.push(match f.has_rail {
        None => CheckItem::warn(
            "layout",
            format!(
                "no `{layout_name}` layout file found — the rail won't appear; run `zj-radar setup zellij --inject` to create one"
            ),
        ),
        Some(true) => CheckItem::ok("layout", format!("`{layout_name}` layout has the radar rail")),
        Some(false) => CheckItem::missing(
            "layout",
            format!(
                "`{layout_name}` layout does not have the radar rail — run `zj-radar setup zellij` or paste the snippet"
            ),
        ),
    });

    // 4. grant
    items.push(match f.granted {
        None => CheckItem::warn("grant", "no permissions.kdl found — run `zj-radar setup zellij -y` to pre-authorize"),
        Some(true) => CheckItem::ok("grant", "wasm is granted in permissions.kdl"),
        Some(false) => CheckItem::missing("grant", "wasm not granted — run `zj-radar setup zellij -y` to pre-authorize (or `--grant` from inside Zellij)"),
    });

    // 5. producer — diagnosis, not a guess: say WHICH producer the doctor saw.
    items.push(match (f.codex_producer, f.claude_producer) {
        (true, true) => CheckItem::ok("producer", "Codex hooks and Claude plugin wired"),
        (true, false) => CheckItem::ok("producer", "Codex hooks wired"),
        (false, true) => CheckItem::ok("producer", "Claude plugin wired"),
        (false, false) => CheckItem::missing(
            "producer",
            "no producer detected — run `zj-radar setup claude` or `zj-radar setup codex`",
        ),
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

/// Zellij's config precedence puts the `ZELLIJ_CONFIG_FILE` env var above
/// config-dir resolution, and setup's resolver honors `ZELLIJ_CONFIG_DIR`/XDG
/// but not this var — so setup can edit a config.kdl Zellij never reads while
/// the doctor reports healthy. Warn when the var points somewhere other than
/// the resolved config path. Pure (env read by the caller), mirroring
/// `resolve_config_dir`.
pub(crate) fn zellij_config_file_item(
    zellij_config_file: Option<std::ffi::OsString>,
    resolved: &std::path::Path,
) -> Option<CheckItem> {
    let file = std::path::PathBuf::from(zellij_config_file.filter(|v| !v.is_empty())?);
    if file == resolved {
        return None;
    }
    Some(CheckItem::warn(
        "config env",
        format!(
            "Zellij will read $ZELLIJ_CONFIG_FILE={}, not {} — unset it or point setup at it",
            file.display(),
            resolved.display()
        ),
    ))
}

/// Returns true when any item is `Missing` — see [`check_codex`]. The claude
/// producer is wired through Claude Code's plugin marketplace, so the doctor
/// has exactly two facts to verify: the binary and the installed plugin.
pub(crate) fn check_claude() -> bool {
    let wired =
        crate::run::claude_producer_wired(claude_installed_plugins_text().as_deref());
    let items = claude_check_items(which("claude"), wired);
    println!("claude:");
    print_check_items(&items)
}

pub(crate) fn claude_check_items(on_path: bool, wired: bool) -> Vec<CheckItem> {
    vec![
        if on_path {
            CheckItem::ok("claude binary", "found on PATH")
        } else {
            CheckItem::missing("claude binary", "not found on PATH")
        },
        if wired {
            CheckItem::ok("plugin", format!("{CLAUDE_PLUGIN} plugin installed"))
        } else {
            CheckItem::missing("plugin", format!("{CLAUDE_PLUGIN} not installed — run `zj-radar setup claude`"))
        },
    ]
}

/// Returns true when any item is `Missing` — see [`check_codex`].
pub(crate) fn check_zellij(layout_name: Option<&str>) -> bool {
    // No resolvable config dir = nothing to inspect; the refusal is the report.
    let Some(config_dir) = zellij_config_dir_or_report() else { return true };
    let (env, paths) = read_zellij_env(&config_dir, layout_name);
    // The inspected layout's NAME comes back on `paths` — the same resolution
    // the read used, so the report and the read can't name different layouts.
    let mut items = zellij_check_items(&analyze_zellij(&env), &paths.layout_name);
    if let Some(item) =
        zellij_config_file_item(std::env::var_os("ZELLIJ_CONFIG_FILE"), &paths.config_path)
    {
        items.push(item);
    }
    println!("zellij:");
    print_check_items(&items)
}

/// Print the items; report whether any is `Missing`.
fn print_check_items(items: &[CheckItem]) -> bool {
    for item in items {
        let status = match item.level {
            CheckLevel::Ok => "ok",
            CheckLevel::Warn => "warn",
            CheckLevel::Missing => "missing",
            CheckLevel::Note => "note",
        };
        println!("  {status} {}: {}", item.name, item.detail);
    }
    items.iter().any(|i| i.level == CheckLevel::Missing)
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
        // The one-time reminder hook mode carries: Codex only runs hooks the
        // user has trusted via `/hooks`. A Note, not a warn — the doctor can't
        // observe trust state, and an unconditional warn made every healthy
        // install read perma-Warn. (Legacy notify mode has no hooks to trust.)
        items.push(CheckItem::note("hook trust", CODEX_HOOK_TRUST_ADVICE));
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
        // The trust reminder is a Note item, never a warn — an unconditional
        // warn made every healthy install read perma-Warn, and Notes are
        // ignored by the Missing exit-code logic.
        let trust = items.iter().find(|item| item.name == "hook trust").expect("hook trust note");
        assert_eq!(trust.level, CheckLevel::Note);
        assert!(trust.detail.contains("/hooks"), "must point at `/hooks`: {}", trust.detail);
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
            codex_producer:          true,
            claude_producer:         false,
            config_managed:          false,
            zellij_version:          Some("zellij 0.44.3".to_string()),
        }
    }

    fn all_good_check_items() -> Vec<CheckItem> {
        zellij_check_items(&all_good_facts(), "default")
    }

    #[test]
    fn zellij_check_items_all_ok() {
        let items = all_good_check_items();
        assert!(items.iter().any(|i| i.name == "zellij binary" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "alias" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "wasm" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "layout" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "grant" && i.level == CheckLevel::Ok));
        assert!(items.iter().any(|i| i.name == "producer" && i.level == CheckLevel::Ok));
        // managed config not emitted when false
        assert!(!items.iter().any(|i| i.name == "managed config"));
    }

    #[test]
    fn zellij_check_flags_a_missing_or_mismatched_binary() {
        // No zellij on PATH: every other item is moot, and an all-ok report
        // on a zellij-less machine would be a --check lie.
        let absent = ZellijFacts { zellij_version: None, ..all_good_facts() };
        assert!(zellij_check_items(&absent, "default")
            .iter()
            .any(|i| i.name == "zellij binary" && i.level == CheckLevel::Missing));
        // Below the floor: warn (advice), not missing — version parsing is
        // best-effort and a working install must not be failed by it.
        let old = ZellijFacts {
            zellij_version: Some("zellij 0.43.1".to_string()),
            ..all_good_facts()
        };
        assert!(zellij_check_items(&old, "default")
            .iter()
            .any(|i| i.name == "zellij binary" && i.level == CheckLevel::Warn));
    }

    #[test]
    fn zellij_version_gate_is_lenient_on_unparseable_output() {
        assert!(zellij_version_is_supported("zellij 0.44.3"));
        assert!(zellij_version_is_supported("0.44.10"));
        // Same minor but predates the swap-layout pinning fix: the plugin
        // loads, the sidebar pops out during layout cycling — must warn.
        assert!(!zellij_version_is_supported("zellij 0.44.1"));
        assert!(!zellij_version_is_supported("0.44.0"));
        assert!(!zellij_version_is_supported("zellij 0.43.1"));
        // Newer releases keep compiled plugins working — the gate is a floor,
        // not a pinned minor, so later versions must pass.
        assert!(zellij_version_is_supported("zellij 0.45.0"));
        assert!(zellij_version_is_supported("zellij 1.0.0"));
        // Numeric comparison, not a starts_with-style prefix check: 0.440 is
        // genuinely above the floor, 0.4.4 genuinely below it.
        assert!(zellij_version_is_supported("zellij 0.440.1"));
        assert!(!zellij_version_is_supported("zellij 0.4.4"));
        // No version-like token at all: err on the side of supported.
        assert!(zellij_version_is_supported("zellij (unknown build)"));
        // Floor minor with unparseable patch digits: likewise lenient.
        assert!(zellij_version_is_supported("zellij 0.44.x"));
    }

    #[test]
    fn zellij_check_items_nix_store_alias_warns() {
        let mut f = all_good_facts();
        f.alias_is_store_path = true;
        let items = zellij_check_items(&f, "default");
        let alias = items.iter().find(|i| i.name == "alias").expect("alias item");
        assert_eq!(alias.level, CheckLevel::Warn, "nix-store alias must warn");
        assert!(alias.detail.contains("nix/store"), "warn detail must mention nix/store");
        assert!(alias.detail.contains("rebuild"), "warn detail must mention rebuild");
    }

    #[test]
    fn zellij_check_items_rail_less_layout_is_missing() {
        let mut f = all_good_facts();
        f.has_rail = Some(false);
        let items = zellij_check_items(&f, "default");
        let layout_item = items.iter().find(|i| i.name == "layout").expect("layout item");
        assert_eq!(layout_item.level, CheckLevel::Missing, "layout without rail must be missing");
        assert!(layout_item.detail.contains("setup zellij"), "hint must mention setup zellij");
    }

    #[test]
    fn zellij_check_items_ungranted_wasm_is_missing() {
        let mut f = all_good_facts();
        f.granted = Some(false);
        let items = zellij_check_items(&f, "default");
        let grant = items.iter().find(|i| i.name == "grant").expect("grant item");
        assert_eq!(grant.level, CheckLevel::Missing, "ungranted wasm must be missing");
        assert!(grant.detail.contains("--grant"), "hint must mention --grant");
    }

    #[test]
    fn zellij_check_items_managed_config_warns() {
        let mut f = all_good_facts();
        f.config_managed = true;
        let items = zellij_check_items(&f, "default");
        let managed = items.iter().find(|i| i.name == "managed config").expect("managed config item");
        assert_eq!(managed.level, CheckLevel::Warn, "managed config must warn");
        assert!(managed.detail.contains("symlink"), "warn detail must mention symlink");
    }

    #[test]
    fn zellij_check_items_missing_alias_is_missing() {
        let mut f = all_good_facts();
        f.unmanaged_alias_present = false;
        let items = zellij_check_items(&f, "default");
        let alias = items.iter().find(|i| i.name == "alias").expect("alias item");
        assert_eq!(alias.level, CheckLevel::Missing);
    }

    #[test]
    fn zellij_check_items_no_layout_warns() {
        let mut f = all_good_facts();
        f.has_rail = None;
        let items = zellij_check_items(&f, "default");
        let layout_item = items.iter().find(|i| i.name == "layout").expect("layout item");
        assert_eq!(layout_item.level, CheckLevel::Warn, "missing layout file should warn");
    }

    #[test]
    fn zellij_check_items_no_permissions_warns() {
        let mut f = all_good_facts();
        f.granted = None;
        let items = zellij_check_items(&f, "default");
        let grant = items.iter().find(|i| i.name == "grant").expect("grant item");
        assert_eq!(grant.level, CheckLevel::Warn, "no permissions.kdl should warn");
    }

    #[test]
    fn zellij_check_items_no_producer_is_missing() {
        let mut f = all_good_facts();
        f.codex_producer = false;
        let items = zellij_check_items(&f, "default");
        let producer = items.iter().find(|i| i.name == "producer").expect("producer item");
        assert_eq!(producer.level, CheckLevel::Missing);
        assert!(producer.detail.contains("setup codex"), "hint must mention setup codex");
    }

    #[test]
    fn zellij_config_file_item_warns_only_on_a_mismatched_override() {
        use std::ffi::OsString;
        let resolved = std::path::Path::new("/home/u/.config/zellij/config.kdl");
        // Unset or empty: no override in play, no item.
        assert_eq!(zellij_config_file_item(None, resolved), None);
        assert_eq!(zellij_config_file_item(Some(OsString::new()), resolved), None);
        // Pointing at the resolved path: consistent, no item.
        assert_eq!(
            zellij_config_file_item(
                Some(OsString::from("/home/u/.config/zellij/config.kdl")),
                resolved,
            ),
            None
        );
        // Pointing elsewhere: Zellij reads a file setup never edits — warn,
        // naming both paths so the fix is actionable.
        let item = zellij_config_file_item(Some(OsString::from("/etc/zellij/config.kdl")), resolved)
            .expect("mismatched override must produce an item");
        assert_eq!(item.level, CheckLevel::Warn, "advice, not a broken install");
        assert!(item.detail.contains("/etc/zellij/config.kdl"), "must name the override");
        assert!(item.detail.contains("/home/u/.config/zellij/config.kdl"), "must name the resolved path");
        assert!(item.detail.contains("unset it"), "must say how to fix it");
    }

    #[test]
    fn zellij_check_items_order_is_stable() {
        let items = all_good_check_items();
        let names: Vec<&str> = items.iter().map(|i| i.name).collect();
        assert_eq!(names, &["zellij binary", "alias", "wasm", "layout", "grant", "producer"]);
    }

    #[test]
    fn zellij_check_layout_item_names_the_inspected_layout() {
        // `--layout mine` used to report on "default layout" regardless — the
        // item must name the layout the doctor actually read, in every arm.
        for has_rail in [Some(true), Some(false), None] {
            let f = ZellijFacts { has_rail, ..all_good_facts() };
            let items = zellij_check_items(&f, "mine");
            let layout_item = items.iter().find(|i| i.name == "layout").expect("layout item");
            assert!(
                layout_item.detail.contains("`mine`"),
                "has_rail={has_rail:?}: detail must name the layout: {}",
                layout_item.detail
            );
        }
    }

    #[test]
    fn claude_check_items_cover_binary_and_plugin() {
        // Mirrors the codex item tests: both facts good → both ok. The plugin
        // detail interpolates the shared CLAUDE_PLUGIN const, so assert against
        // it rather than re-pinning the literal here.
        let items = claude_check_items(true, true);
        assert!(items.contains(&CheckItem::ok("claude binary", "found on PATH")));
        let plugin = items.iter().find(|i| i.name == "plugin").expect("plugin item");
        assert_eq!(plugin.level, CheckLevel::Ok);
        assert!(
            plugin.detail.contains(CLAUDE_PLUGIN),
            "plugin item must name the plugin: {}",
            plugin.detail
        );

        // Binary absent → missing.
        let items = claude_check_items(false, true);
        assert!(items.contains(&CheckItem::missing("claude binary", "not found on PATH")));

        // Plugin absent → missing, with the remedy pinned so the hint the
        // doctor prints stays a real invocation.
        let items = claude_check_items(true, false);
        let plugin = items.iter().find(|i| i.name == "plugin").expect("plugin item");
        assert_eq!(plugin.level, CheckLevel::Missing);
        assert!(
            plugin.detail.contains("run `zj-radar setup claude`"),
            "remedy wording must name the setup command: {}",
            plugin.detail
        );
    }

    /// Every backtick-quoted `zj-radar …` invocation the doctor prints as a
    /// remedy must parse as a real CLI invocation — a hint that clap rejects
    /// sends the user into a usage error instead of a fix.
    #[test]
    fn remedy_hints_parse_as_real_invocations() {
        use clap::Parser;
        let all_missing = || ZellijFacts {
            unmanaged_alias_present: false,
            wasm_present: false,
            has_rail: Some(false),
            granted: Some(false),
            codex_producer: false,
            claude_producer: false,
            zellij_version: None,
            ..all_good_facts()
        };
        let warn_variants = ZellijFacts { has_rail: None, granted: None, ..all_missing() };

        let mut details: Vec<String> = Vec::new();
        for items in [
            zellij_check_items(&all_missing(), "default"),
            zellij_check_items(&warn_variants, "default"),
            claude_check_items(false, false),
            codex_check_items(
                &analyze_codex(&CodexEnv {
                    codex_on_path:    false,
                    zj_radar_on_path: false,
                    config_text:      None,
                    hooks_text:       None,
                }),
                false,
            ),
        ] {
            details.extend(items.into_iter().map(|i| i.detail));
        }

        let invocations: Vec<&str> = details
            .iter()
            .flat_map(|d| d.split('`').skip(1).step_by(2)) // backticked spans
            .filter(|s| s.starts_with("zj-radar "))
            .collect();
        assert!(
            invocations.contains(&"zj-radar setup zellij -y"),
            "the grant remedy must be among the extracted hints: {invocations:?}"
        );
        for invocation in invocations {
            let argv: Vec<&str> = invocation.split_whitespace().collect();
            assert!(
                crate::Cli::try_parse_from(&argv).is_ok(),
                "remedy hint `{invocation}` must parse as a real CLI invocation"
            );
        }
    }
}
