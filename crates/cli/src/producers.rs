//! Producer detection: which instrumented agents are wired to push status.
//!
//! One home for every detection route, read by `run`'s advisory, the doctor's
//! `producer` item, and `setup zellij`'s epilogue — so "is agent X wired?"
//! cannot drift between them. Each route is a marker check over a config file
//! the matching `setup <agent>` writes; the evidence is gathered once
//! ([`ProducerTexts::read`]) and graded purely ([`ProducerTexts::wired`]).

use crate::agents::Agent;
use crate::setup::{CODEX_HOOK_MARKER, CLAUDE_PLUGIN};

/// Three producers, three wiring routes — name all, because `zj-radar setup`
/// wires each agent symmetrically (claude drives Claude Code's plugin
/// marketplace; opencode drops a vendored JS bridge into its plugins dir).
pub(crate) const PRODUCER_HINT: &str = "Agent status off — no producer wired. Run `zj-radar setup claude` \
    (Claude Code), `zj-radar setup codex` (Codex), or `zj-radar setup opencode` (Opencode).";

/// The per-agent evidence, already read. `None` = the file is absent.
#[derive(Default)]
pub(crate) struct ProducerTexts {
    /// Codex's `hooks.json`; wired when it carries our command-hook marker.
    pub codex_hooks:     Option<String>,
    /// Claude Code's `installed_plugins.json`; wired when it names our plugin.
    pub claude_plugins:  Option<String>,
    /// opencode's vendored bridge plugin; wired when it carries our header marker.
    pub opencode_plugin: Option<String>,
}

impl ProducerTexts {
    /// The one IO point: read every producer's evidence from its home.
    pub(crate) fn read() -> Self {
        ProducerTexts {
            codex_hooks:     crate::setup::codex_hooks_text(),
            claude_plugins:  crate::setup::claude_installed_plugins_text(),
            opencode_plugin: crate::setup::opencode_plugin_text(),
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
            Agent::Codex => self.codex_hooks.as_deref().is_some_and(|h| h.contains(CODEX_HOOK_MARKER)),
            Agent::Claude => self.claude_plugins.as_deref().is_some_and(|p| p.contains(CLAUDE_PLUGIN)),
            Agent::Opencode => self
                .opencode_plugin
                .as_deref()
                .is_some_and(crate::setup::detect::opencode_plugin_is_ours),
        }
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
    use crate::setup::OPENCODE_PLUGIN_MARKER;

    fn texts(codex: bool, claude: bool, opencode: bool) -> ProducerTexts {
        ProducerTexts {
            codex_hooks:     codex.then(|| format!("{{\"command\": \"{CODEX_HOOK_MARKER} zj-radar notify codex\"}}")),
            claude_plugins:  claude.then(|| format!("{{\"plugins\":[\"{CLAUDE_PLUGIN}\"]}}")),
            opencode_plugin: opencode.then(|| format!("// {OPENCODE_PLUGIN_MARKER}\n")),
        }
    }

    #[test]
    fn wired_lists_agents_in_declaration_order() {
        assert_eq!(texts(true, true, true).wired(), Agent::ALL.to_vec());
        assert_eq!(texts(true, false, true).wired(), vec![Agent::Codex, Agent::Opencode]);
        assert!(texts(false, false, false).wired().is_empty());
    }

    #[test]
    fn each_route_keys_on_its_marker_not_on_file_presence() {
        let foreign = ProducerTexts {
            codex_hooks:     Some("{\"command\": \"/other/notifier\"}".to_string()),
            claude_plugins:  Some("{\"plugins\":[\"someone-else\"]}".to_string()),
            opencode_plugin: Some("// some other plugin\n".to_string()),
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
