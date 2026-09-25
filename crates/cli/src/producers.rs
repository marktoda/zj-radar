//! Producer detection: which instrumented agents are wired to push status.
//!
//! One home for every detection route, read by `run`'s advisory, the doctor's
//! `producer` item, and `setup zellij`'s epilogue — so "is agent X wired?"
//! cannot drift between them. Each route is a marker check over a config file
//! the matching `setup <agent>` writes; the evidence is gathered once
//! ([`ProducerTexts::read`]) and graded purely ([`ProducerTexts::wired`]).

use crate::agents::Agent;
use crate::setup::{CODEX_HOOK_MARKER, CLAUDE_PLUGIN};

/// Four producers, four wiring routes — name all, because `zj-radar setup`
/// wires each agent symmetrically (claude drives Claude Code's plugin
/// marketplace; opencode and pi each drop a vendored JS bridge into their
/// auto-loaded dirs).
pub(crate) const PRODUCER_HINT: &str = "Agent status off — no producer wired. Run `zj-radar setup claude` \
    (Claude Code), `zj-radar setup codex` (Codex), `zj-radar setup opencode` (Opencode), or \
    `zj-radar setup pi` (pi).";

/// The per-agent evidence, already read. `None` = the file is absent.
#[derive(Default)]
pub(crate) struct ProducerTexts {
    /// Codex's `hooks.json`; wired when it carries our command-hook marker.
    pub codex_hooks:     Option<String>,
    /// Codex's `config.toml`; wired when its top-level `notify` is ours (the
    /// `setup codex --legacy-notify` route).
    pub codex_config:    Option<String>,
    /// Claude Code's `installed_plugins.json`; wired when it names our plugin.
    pub claude_plugins:  Option<String>,
    /// opencode's vendored 1.x bridge plugin; wired when it carries our header marker.
    pub opencode_plugin: Option<String>,
    /// opencode's vendored 2.x TUI bridge plugin; same marker test. Either
    /// bridge being ours counts as wired — which one opencode loads is its
    /// version's business, not ours.
    pub opencode_tui_plugin: Option<String>,
    /// pi's vendored bridge extension; wired when it carries our header marker.
    pub pi_extension: Option<String>,
}

impl ProducerTexts {
    /// The one IO point: read every producer's evidence from its home.
    pub(crate) fn read() -> Self {
        ProducerTexts {
            codex_hooks:         crate::setup::codex_hooks_text(),
            codex_config:        crate::setup::codex_config_text(),
            claude_plugins:      crate::setup::claude_installed_plugins_text(),
            opencode_plugin:     crate::setup::opencode_plugin_text(),
            opencode_tui_plugin: crate::setup::opencode_tui_plugin_text(),
            pi_extension:        crate::setup::pi_extension_text(),
        }
    }

    /// The wired agents, in [`Agent::ALL`] order.
    pub(crate) fn wired(&self) -> Vec<Agent> {
        Agent::ALL.iter().copied().filter(|a| self.is_wired(*a)).collect()
    }

    /// Exhaustive on purpose: a new `Agent` variant does not compile until its
    /// detection route is declared here — the one wiring point the agent
    /// guard lattice can't otherwise reach.
    pub(crate) fn is_wired(&self, agent: Agent) -> bool {
        match agent {
            Agent::Codex => self.codex_via_hooks() || self.codex_via_notify(),
            Agent::Claude => self.claude_plugins.as_deref().is_some_and(|p| p.contains(CLAUDE_PLUGIN)),
            Agent::Opencode => [&self.opencode_plugin, &self.opencode_tui_plugin]
                .into_iter()
                .any(|text| text.as_deref().is_some_and(crate::setup::detect::opencode_plugin_is_ours)),
            Agent::Pi => self.pi_extension.as_deref().is_some_and(crate::setup::detect::pi_extension_is_ours),
        }
    }

    /// Codex wired through `hooks.json` (the default `setup codex` route).
    pub(crate) fn codex_via_hooks(&self) -> bool {
        self.codex_hooks.as_deref().is_some_and(|h| h.contains(CODEX_HOOK_MARKER))
    }

    /// Codex wired through our `config.toml` notify slot (`--legacy-notify`).
    pub(crate) fn codex_via_notify(&self) -> bool {
        self.codex_config.as_deref().is_some_and(crate::setup::detect::codex_config_notify_is_ours)
    }

    /// The `setup` re-runs that refresh exactly the wiring already in place
    /// (`update` after a CLI move): every wired agent in one `setup … -y`,
    /// except a notify-only Codex, which needs its own `--legacy-notify` run
    /// — plain `setup codex` would install hooks the user never chose.
    pub(crate) fn rewire_invocations(&self) -> Vec<Vec<&'static str>> {
        let wired = self.wired();
        let notify_only = wired.contains(&Agent::Codex) && !self.codex_via_hooks();
        let mut runs = Vec::new();
        let rest: Vec<&'static str> =
            wired.iter().filter(|a| !(notify_only && **a == Agent::Codex)).map(|a| a.source()).collect();
        if !rest.is_empty() {
            let mut args = vec!["setup"];
            args.extend(rest);
            args.push("-y");
            runs.push(args);
        }
        if notify_only {
            runs.push(vec!["setup", "codex", "--legacy-notify", "-y"]);
        }
        runs
    }
}

/// `Some(hint)` when no producer is wired, else `None`.
pub(crate) fn producer_hint(wired: &[Agent]) -> Option<String> {
    wired.is_empty().then(|| PRODUCER_HINT.to_string())
}

/// "codex, opencode" — the wired agents by name, for status lines.
pub(crate) fn names(agents: &[Agent]) -> String {
    agents.iter().map(|a| a.source()).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{OPENCODE_PLUGIN_MARKER, OPENCODE_TUI_PLUGIN_MARKER, PI_EXTENSION_MARKER};

    fn texts(codex: bool, claude: bool, opencode: bool, pi: bool) -> ProducerTexts {
        ProducerTexts {
            codex_hooks:         codex.then(|| format!("{{\"command\": \"{CODEX_HOOK_MARKER} zj-radar notify codex\"}}")),
            codex_config:        None,
            claude_plugins:      claude.then(|| format!("{{\"plugins\":[\"{CLAUDE_PLUGIN}\"]}}")),
            opencode_plugin:     opencode.then(|| format!("// {OPENCODE_PLUGIN_MARKER}\n")),
            opencode_tui_plugin: None,
            pi_extension:        pi.then(|| format!("// {PI_EXTENSION_MARKER}\n")),
        }
    }

    #[test]
    fn opencode_is_wired_by_either_bridge_file() {
        // A 2.x-only install (the TUI plugin dir, no 1.x file) is wired.
        let tui_only = ProducerTexts {
            opencode_tui_plugin: Some(format!("// {OPENCODE_TUI_PLUGIN_MARKER}\n")),
            ..ProducerTexts::default()
        };
        assert_eq!(tui_only.wired(), vec![Agent::Opencode]);
        // A foreign TUI plugin beside our 1.x file: still wired, by the 1.x file.
        let mixed = ProducerTexts {
            opencode_plugin:     Some(format!("// {OPENCODE_PLUGIN_MARKER}\n")),
            opencode_tui_plugin: Some("export default { id: \"other\", setup() {} };\n".to_string()),
            ..ProducerTexts::default()
        };
        assert_eq!(mixed.wired(), vec![Agent::Opencode]);
    }

    const OUR_NOTIFY: &str = "notify = [\"zj-radar\", \"notify\", \"codex\"]\nmodel = \"x\"\n";

    #[test]
    fn codex_is_wired_by_our_legacy_notify_slot() {
        // `setup codex --legacy-notify` writes no hooks.json: our notify slot
        // alone must count, or the doctor/`run`/`update` call Codex unwired.
        let notify_only = ProducerTexts { codex_config: Some(OUR_NOTIFY.to_string()), ..ProducerTexts::default() };
        assert_eq!(notify_only.wired(), vec![Agent::Codex]);
        assert!(notify_only.codex_via_notify() && !notify_only.codex_via_hooks());
        // An unparseable config.toml owns nothing.
        let broken = ProducerTexts { codex_config: Some("notify = = [".to_string()), ..ProducerTexts::default() };
        assert!(broken.wired().is_empty());
    }

    #[test]
    fn rewire_keeps_each_codex_route_as_installed() {
        // Notify-only Codex re-wires with --legacy-notify, in its own run.
        let notify_only = ProducerTexts {
            codex_config: Some(OUR_NOTIFY.to_string()),
            ..texts(false, true, false, false)
        };
        assert_eq!(
            notify_only.rewire_invocations(),
            vec![vec!["setup", "claude", "-y"], vec!["setup", "codex", "--legacy-notify", "-y"]]
        );
        let alone = ProducerTexts { codex_config: Some(OUR_NOTIFY.to_string()), ..ProducerTexts::default() };
        assert_eq!(alone.rewire_invocations(), vec![vec!["setup", "codex", "--legacy-notify", "-y"]]);
        // Hooks present (with or without the notify slot): the default route.
        let both = ProducerTexts { codex_config: Some(OUR_NOTIFY.to_string()), ..texts(true, false, false, true) };
        assert_eq!(both.rewire_invocations(), vec![vec!["setup", "codex", "pi", "-y"]]);
        assert!(ProducerTexts::default().rewire_invocations().is_empty());
    }

    #[test]
    fn wired_lists_agents_in_declaration_order() {
        assert_eq!(texts(true, true, true, true).wired(), Agent::ALL.to_vec());
        assert_eq!(texts(true, false, true, false).wired(), vec![Agent::Codex, Agent::Opencode]);
        assert!(texts(false, false, false, false).wired().is_empty());
    }

    #[test]
    fn each_route_keys_on_its_marker_not_on_file_presence() {
        let foreign = ProducerTexts {
            codex_hooks:         Some("{\"command\": \"/other/notifier\"}".to_string()),
            codex_config:        Some("notify = [\"my-notifier\", \"--flag\"]\n".to_string()),
            claude_plugins:      Some("{\"plugins\":[\"someone-else\"]}".to_string()),
            opencode_plugin:     Some("// some other plugin\n".to_string()),
            opencode_tui_plugin: Some("export default { id: \"other\", setup() {} };\n".to_string()),
            pi_extension:        Some("// some other extension\n".to_string()),
        };
        assert!(foreign.wired().is_empty(), "present-but-foreign files are not wired");
        assert!(ProducerTexts::default().wired().is_empty(), "absent files are not wired");
    }

    #[test]
    fn hint_only_when_nothing_is_wired_and_names_every_route() {
        assert!(producer_hint(&[Agent::Codex]).is_none());
        let hint = producer_hint(&[]).unwrap();
        for agent in Agent::ALL {
            let route = format!("zj-radar setup {}", agent.source());
            assert!(hint.contains(&route), "hint must name the {route} route: {hint}");
        }
    }

    #[test]
    fn names_joins_sources() {
        assert_eq!(names(&[Agent::Claude, Agent::Opencode]), "claude, opencode");
        assert_eq!(names(&[]), "");
    }
}
