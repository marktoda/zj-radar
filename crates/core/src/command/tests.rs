    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn display_command_keeps_useful_subcommands_and_drops_flags() {
        assert_eq!(
            display_command(&argv(&[
                "cargo",
                "test",
                "render::tests",
                "--features",
                "cli",
                "--",
                "--nocapture"
            ])),
            "cargo test render::tests"
        );
        assert_eq!(
            display_command(&argv(&[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5"
            ])),
            "codex"
        );
        assert_eq!(
            display_command(&argv(&["npm", "run", "build", "--", "--watch"])),
            "npm run build"
        );
        assert_eq!(
            display_command(&argv(&["python", "-m", "pytest", "-q", "tests/render.rs"])),
            "python -m pytest tests/render.rs"
        );
        assert_eq!(display_command(&argv(&["sleep", "5"])), "sleep 5");
    }

    /// Pin `display_command` output across every branch of the table-driven
    /// model: each `ToolRule` shape (`subcommands: Some` known-sub lookup vs
    /// `None` first-arg), in/out of `target_verbs`, the `FIRST_ARG_RULE`
    /// fallback (proving the former pytest|ruff|make|just arm still resolves
    /// natively), the `python -m` path, agents, and the unknown/bare-exe
    /// collapse. One case per code path so the refactor's equivalence is
    /// verifiable, not merely asserted.
    #[test]
    fn display_command_covers_every_tool_rule_path() {
        let cases: &[(&[&str], &str)] = &[
            // cargo: known-subcommand lookup; target appended only for target_verbs.
            (&["cargo", "build"], "cargo build"),               // sub, not a target verb
            (&["cargo", "clippy", "--fix"], "cargo clippy"),    // flags dropped
            (&["cargo", "test", "auth::cases"], "cargo test auth::cases"), // target verb + target
            (&["cargo", "run", "--release"], "cargo run"),      // target verb, target is an option → none
            (&["cargo", "nextest", "run"], "cargo nextest run"),// nextest is a target verb
            (&["cargo", "frobnicate"], "cargo"),                // unknown sub → bare exe
            (&["cargo"], "cargo"),                              // no args → bare exe
            // go: same family as cargo, narrower sub/target sets.
            (&["go", "test", "./..."], "go test ./..."),        // target verb + target
            (&["go", "build", "./cmd/app"], "go build"),        // sub, not a target verb
            (&["go", "mod", "tidy"], "go mod"),                 // sub, not a target verb
            // npm family: first-arg verb; only `run` takes a target.
            (&["yarn", "run", "dev"], "yarn run dev"),          // run + script
            (&["npm", "ci"], "npm ci"),                         // first-arg, not `run`
            (&["pnpm", "install", "left-pad"], "pnpm install"), // first-arg keeps verb only
            (&["bun"], "bun"),                                  // no args → bare exe
            // FIRST_ARG_RULE fallback — the former pytest|ruff|make|just arm.
            (&["make", "deploy"], "make deploy"),
            (&["make"], "make"),
            (&["just", "serve"], "just serve"),
            (&["ruff", "check", "."], "ruff check"),            // target_verbs empty → no target
            (&["pytest", "tests/unit.py"], "pytest tests/unit.py"),
            (&["pytest", "-q"], "pytest"),                      // only options → bare exe
            (&["tail", "-f", "app.log"], "tail app.log"),       // first NON-option is the verb
            (&["htop"], "htop"),                                // truly unknown, no args
            // Agents: bare name regardless of args (push-owned panes).
            (&["claude", "--model", "opus"], "claude"),
            (&["gemini", "chat"], "gemini"),
            // python: dedicated `-m` shape with a pytest sub-target.
            (&["python", "-m", "pytest", "-q", "t.py"], "python -m pytest t.py"),
            (&["python", "-m", "http.server"], "python -m http.server"),
            (&["python", "-m"], "python"),                      // `-m` with no module → bare exe
            (&["python", "path/to/app.py", "--v"], "python app.py"), // script basename
            (&["python3"], "python3"),                          // no args → bare exe
            // Deliberate post-refactor behavior: the uniform target dash-guard
            // drops a bare "-" target (old cargo nextest / go test kept it).
            (&["go", "test", "-"], "go test"),
            (&["cargo", "nextest", "-"], "cargo nextest"),
        ];
        for (args, want) in cases {
            assert_eq!(&display_command(&argv(args)), want, "display for {args:?}");
        }
    }

    /// Representative `(argv, display, expected Kind)` covering every `Kind`
    /// that `command_kind` can emit. Shared by the classification test and the
    /// Kind-round-trip guard so both exercise exactly the same set.
    fn kind_classification_cases() -> Vec<(Vec<String>, &'static str, Kind)> {
        use crate::kind::Kind;
        vec![
            // Agents, by basename.
            (argv(&["claude"]), "claude", Kind::Claude),
            (argv(&["codex", "--dangerously-bypass-sandbox"]), "codex", Kind::Codex),
            (argv(&["gemini"]), "gemini", Kind::Gemini),
            // Test runners across ecosystems.
            (argv(&["cargo", "test", "--features", "cli"]), "cargo test", Kind::Test),
            (argv(&["pytest"]), "pytest", Kind::Test),
            (argv(&["go", "test", "./..."]), "go test ./...", Kind::Test),
            (argv(&["npm", "run", "test"]), "npm run test", Kind::Test),
            // Build.
            (argv(&["cargo", "build"]), "cargo build", Kind::Build),
            (argv(&["npm", "run", "build"]), "npm run build", Kind::Build),
            // Server and deploy (npm dev-server; make/just verb routing).
            (argv(&["npm", "run", "dev"]), "npm run dev", Kind::Server),
            (argv(&["just", "serve"]), "just serve", Kind::Server),
            (argv(&["make", "deploy"]), "make deploy", Kind::Deploy),
            // Anything unrecognized is a plain command.
            (argv(&["sleep", "5"]), "sleep 5", Kind::Command),
        ]
    }

    #[test]
    fn wrappers_and_env_prefixes_are_peeled_before_classification() {
        // Real Zellij argv routinely carries env assignments and launcher
        // wrappers. The observer must classify the *wrapped* command, not the
        // wrapper, for both the display string and the Kind.
        let cases: &[(&[&str], &str, Kind)] = &[
            (&["RUST_LOG=debug", "cargo", "test", "render"], "cargo test render", Kind::Test),
            (&["sudo", "cargo", "build"], "cargo build", Kind::Build),
            (&["env", "FOO=1", "BAR=2", "pytest"], "pytest", Kind::Test),
            (&["time", "npm", "run", "build"], "npm run build", Kind::Build),
        ];
        for (args, want_msg, want_kind) in cases {
            let mut store = CommandStore::default();
            store.on_command_changed(1, &argv(args), true, Some("/work/repo"), 1);
            store.on_timer(2);
            let s = store
                .get(1)
                .unwrap_or_else(|| panic!("{args:?} should be tracked"));
            assert_eq!(&s.msg, want_msg, "display for {args:?}");
            assert_eq!(s.kind, *want_kind, "kind for {args:?}");
        }
    }

    #[test]
    fn a_wrapped_agent_is_still_suppressed() {
        // `sudo claude` is still claude — a push-owned agent — so it must not
        // open a command lifecycle even behind a wrapper.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &argv(&["sudo", "claude"]), true, Some("/work/repo"), 1);
        store.on_timer(2);
        assert!(store.get(1).is_none(), "wrapped agent must stay suppressed");
    }

    #[test]
    fn unknown_wrapper_options_are_left_alone() {
        // `sudo -u user make` carries a value-taking option we don't model;
        // rather than mis-parse it, peeling bails and leaves the command as-is
        // (no regression vs. not peeling). It still tracks as a generic command.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &argv(&["sudo", "-u", "user", "make"]), true, Some("/r"), 1);
        store.on_timer(2);
        let s = store.get(1).expect("should still be tracked");
        assert_eq!(s.kind, Kind::Command);
    }

    #[test]
    fn command_kind_classifies_every_emitted_kind() {
        for (cmd, display, expected) in kind_classification_cases() {
            assert_eq!(command_kind(&cmd, display), expected, "classify {display:?}");
        }
    }

    #[test]
    fn command_source_round_trips_through_kind() {
        // Twin of the agent-side `source_round_trips_through_kind` (see
        // CONTEXT.md "Information source"). The command path stores
        // `command_kind(..).as_source()` and the roll-up reads it back via
        // `Kind::from_source`, so every classified command must survive that
        // round-trip to the SAME kind — and never degrade to `Kind::Other`,
        // the reserved sentinel for a genuinely-unknown source. (Kind's own
        // universal round-trip is guarded in `kind.rs`; this pins that the
        // command boundary actually rides that seam.)
        use crate::kind::Kind;
        for (cmd, display, _) in kind_classification_cases() {
            let kind = command_kind(&cmd, display);
            assert_ne!(kind, Kind::Other, "{display:?} classified as the Other sentinel");
            assert_eq!(
                Kind::from_source(kind.as_source()),
                kind,
                "{display:?} source {:?} must round-trip to its kind",
                kind.as_source(),
            );
        }
    }

    #[test]
    fn resolved_command_source_round_trips_through_kind() {
        // End-to-end twin: drive a command through the store and confirm the
        // *persisted* observation `source` (not just the classifier output)
        // round-trips to the kind the classifier picked. Guards the wiring in
        // `on_command_changed` → `on_timer`, not only `command_kind` in
        // isolation.
        use crate::kind::Kind;
        let mut store = CommandStore::default();
        let cmd = argv(&["cargo", "test", "--features", "cli"]);
        store.on_command_changed(1, &cmd, true, Some("/home/u/repo"), 1);
        store.on_timer(1 + DEBOUNCE_TICKS);
        let obs = store.get(1).expect("fg command promoted to resolved");
        assert_eq!(obs.kind, Kind::Test);
    }

    // ── Test 1: fg real command → pending, NOT Running until on_timer past DEBOUNCE_TICKS

    #[test]
    fn fg_command_stays_pending_until_debounce() {
        let mut store = CommandStore::default();
        let cmd = vec!["sleep".to_string(), "5".to_string()];

        // t=1: fg command arrives → pending, not yet Running
        store.on_command_changed(1, &cmd, true, Some("/home/user/myrepo"), 1);
        assert!(
            store.get(1).is_none(),
            "must not be Running yet — still pending"
        );
        assert!(store.pending.contains_key(&1), "must be in pending");

        // t=1: timer fires at same tick → not past debounce (0 < 1)
        store.on_timer(1);
        assert!(store.get(1).is_none(), "still pending at same tick");

        // t=2: timer fires past debounce (2 - 1 = 1 >= DEBOUNCE_TICKS) → promote
        store.on_timer(2);
        let s = store.get(1).expect("must be Running after debounce");
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.msg, "sleep 5");
        assert_eq!(s.kind, Kind::Command);
        assert_eq!(s.repo, "myrepo");
        assert!(
            !store.pending.contains_key(&1),
            "pending cleared after promotion"
        );
    }

    // ── Test 2: fg blip filtered (real command then is_foreground=false before timer)

    #[test]
    fn fg_blip_cleared_before_timer_never_becomes_running() {
        let mut store = CommandStore::default();
        let cmd = vec!["cargo".to_string(), "build".to_string()];

        // t=1: fg real command → pending
        store.on_command_changed(1, &cmd, true, None, 1);
        assert!(store.pending.contains_key(&1));

        // t=1: is_foreground=false (e.g. zellij reports bg) → clear pending
        store.on_command_changed(1, &[], false, None, 1);
        assert!(
            !store.pending.contains_key(&1),
            "pending cleared on return-to-shell"
        );

        // t=5: timer fires — nothing to promote
        store.on_timer(5);
        assert!(store.get(1).is_none(), "must never become Running");
    }

    // ── Test 3: starship ignore-set: stays Idle, no pending, no Done

    #[test]
    fn starship_on_idle_pane_leaves_no_trace() {
        let mut store = CommandStore::default();
        let cmd = vec!["starship".to_string()];

        store.on_command_changed(1, &cmd, true, None, 1);
        assert!(
            !store.pending.contains_key(&1),
            "starship must not enter pending"
        );
        assert!(store.get(1).is_none(), "no resolved state expected");
    }

    #[test]
    fn recede_if_focused_clears_done_command_but_not_error() {
        let mut store = CommandStore::default();
        store.on_exit(1, Some(0), 1); // Done, on_focus = Some(Idle)
        store.on_exit(2, Some(3), 1); // Error, on_focus = Some(Idle)

        store.recede_if_focused(1, 5);
        store.recede_if_focused(2, 5);

        assert_eq!(store.get(1).unwrap().status, Status::Idle, "Done recedes");
        assert_eq!(store.get(2).unwrap().status, Status::Error, "Error persists");
        store.recede_if_focused(999, 5); // unknown id is a no-op
    }

    // ── Test 4: Running → return-to-shell → Done with on_focus; on_pane_focused → Idle

    #[test]
    fn running_to_return_to_shell_sets_done_then_focused_sets_idle() {
        let mut store = CommandStore::default();
        let cmd = vec!["make".to_string()];

        // t=1: fg real command
        store.on_command_changed(1, &cmd, true, Some("/repo"), 1);
        // t=2: promote to Running
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        // t=3: return-to-shell (is_foreground=false) → tentative, still Running
        store.on_command_changed(1, &[], false, None, 3);
        assert_eq!(store.get(1).unwrap().status, Status::Running);
        // t=4: timer past debounce → Done with on_focus=Some(Idle)
        store.on_timer(4);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.on_focus, Some(Status::Idle));
        assert_eq!(s.last_change_tick, 4);

        // t=5: pane focused → Idle, on_focus cleared
        store.on_pane_focused(1, 5);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Idle);
        assert_eq!(s.on_focus, None);
        assert_eq!(s.last_change_tick, 5);
    }

    // ── Test 5: on_exit(Some(0)) → Done; on_exit(Some(3)) → Error; dedupe

    #[test]
    fn on_exit_sets_status_and_dedupes() {
        let mut store = CommandStore::default();

        // Exit 0 → Done with on_focus=Some(Idle)
        store.on_exit(1, Some(0), 5);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.on_focus, Some(Status::Idle));

        // Repeated identical exit → no-op (on_focus unchanged, tick unchanged)
        store.on_exit(1, Some(0), 10);
        let s = store.get(1).unwrap();
        assert_eq!(
            s.last_change_tick, 5,
            "repeated identical exit must be a no-op"
        );

        // Pane 2: nonzero exit → Error
        store.on_exit(2, Some(3), 6);
        let s = store.get(2).unwrap();
        assert_eq!(s.status, Status::Error);
        assert_eq!(s.on_focus, Some(Status::Idle));

        // Repeated identical exit for pane 2 → no-op
        store.on_exit(2, Some(3), 99);
        assert_eq!(
            store.get(2).unwrap().last_change_tick,
            6,
            "repeated identical exit must be a no-op"
        );
    }

    #[test]
    fn rerun_with_same_exit_code_is_not_swallowed_by_dedup() {
        // A held-open command pane (e.g. `zellij run`, or "rerun command pane")
        // keeps its id and stays live, so its `exited` dedup entry survives. When
        // the pane is RE-RUN and exits with the SAME code, the second exit must
        // still resolve to Done — otherwise the row is stuck Running forever and
        // `has_pending_or_active` keeps the timer armed (poll/CPU drain).
        let mut store = CommandStore::default();

        // First run: promote to Running, then exit 0 → Done.
        store.on_command_changed(7, &argv(&["sleep", "5"]), true, Some("/r"), 1);
        store.on_timer(2);
        assert_eq!(store.get(7).unwrap().status, Status::Running);
        store.on_exit(7, Some(0), 3);
        assert_eq!(store.get(7).unwrap().status, Status::Done);

        // Re-run in the same (still-live) pane: back to Running.
        store.on_command_changed(7, &argv(&["sleep", "5"]), true, Some("/r"), 4);
        store.on_timer(5);
        assert_eq!(store.get(7).unwrap().status, Status::Running);

        // Second run exits with the SAME code — must resolve to Done, not stay Running.
        store.on_exit(7, Some(0), 6);
        assert_eq!(
            store.get(7).unwrap().status,
            Status::Done,
            "a re-run's exit must apply even when its code matches the prior run"
        );
        assert!(
            !store.has_pending_or_active(),
            "a finished re-run must not keep the timer armed"
        );
    }

    // ── Test 6: basename of an absolute argv[0] path

    #[test]
    fn absolute_argv0_path_basename_used_for_command_and_repo() {
        let mut store = CommandStore::default();
        // Nix store path for cargo
        let cmd = vec![
            "/nix/store/abc123-cargo-1.0/bin/cargo".to_string(),
            "build".to_string(),
        ];

        store.on_command_changed(1, &cmd, true, Some("/home/user/myproject"), 1);
        store.on_timer(2);
        let s = store.get(1).expect("must be Running");
        assert_eq!(s.msg, "cargo build", "basename of nix path must be used");
        assert_eq!(s.kind, Kind::Build);
        assert_eq!(s.repo, "myproject", "repo must be basename of cwd");
    }

    // ── Test 7: prune drops dead panes from all maps

    #[test]
    fn prune_drops_dead_panes_from_all_maps() {
        let mut store = CommandStore::default();

        // Set up pane 1: pending
        store.on_command_changed(1, &["vim".to_string()], true, None, 1);
        // Set up pane 2: resolved Running
        store.on_command_changed(2, &["cargo".to_string()], true, None, 1);
        store.on_timer(2);
        // Set up pane 3: has exit record
        store.on_exit(3, Some(0), 1);

        // Keep only pane 2
        let live: HashSet<u32> = [2].into_iter().collect();
        store.prune(&live);

        assert!(store.get(1).is_none(), "pane 1 resolved must be pruned");
        assert!(
            !store.pending.contains_key(&1),
            "pane 1 pending must be pruned"
        );
        assert!(store.get(2).is_some(), "pane 2 must survive");
        assert!(store.get(3).is_none(), "pane 3 resolved must be pruned");
        assert!(
            !store.exited.contains_key(&3),
            "pane 3 exited must be pruned"
        );
    }

    // ── Test 8: has_pending_or_active

    #[test]
    fn has_pending_or_active_reflects_state() {
        let mut store = CommandStore::default();
        assert!(!store.has_pending_or_active(), "empty store → false");

        // Add a pending entry
        store.on_command_changed(1, &["vim".to_string()], true, None, 1);
        assert!(store.has_pending_or_active(), "true while pending");

        // Promote to Running
        store.on_timer(2);
        assert!(store.has_pending_or_active(), "true while Running");

        // Return to shell → tentative; still active (Running) until debounce.
        store.on_command_changed(1, &[], false, None, 3);
        assert!(
            store.has_pending_or_active(),
            "still active until the debounce window flips it to Done"
        );

        // Timer past debounce → Done (no pending, no Running).
        store.on_timer(4);
        assert!(
            !store.has_pending_or_active(),
            "false once Done (no pending, no Running)"
        );

        // Focus to clear to Idle
        store.on_pane_focused(1, 5);
        assert!(!store.has_pending_or_active(), "false when Idle");
    }

    // ── Additional edge cases ──

    #[test]
    fn return_to_shell_on_idle_pane_leaves_no_done() {
        // A starship blip on an idle prompt must NOT create a Done entry.
        let mut store = CommandStore::default();

        // Pane is idle (no resolved entry yet); return-to-shell arrives
        store.on_command_changed(1, &[], false, None, 1);
        assert!(
            store.get(1).is_none(),
            "idle + return-to-shell must not create Done"
        );
    }

    #[test]
    fn ignore_set_covers_all_shells() {
        let mut store = CommandStore::default();
        // All shell/prompt names in IGNORE_NAMES must be filtered. "starship" is
        // included because it fires a CommandChanged event before the real shell
        // prompt reappears — treating it as a command would cause a spurious Done.
        for shell in &["zsh", "bash", "fish", "sh", "dash", "starship"] {
            let cmd = vec![shell.to_string()];
            store.on_command_changed(1, &cmd, true, None, 1);
            assert!(
                !store.pending.contains_key(&1),
                "{} must not enter pending",
                shell
            );
            assert!(
                store.get(1).is_none(),
                "{} must leave no resolved state",
                shell
            );
        }
    }

    // ── Test: on_exit(None) → Done with on_focus=Some(Idle), ever_active=true

    #[test]
    fn on_exit_none_yields_done_and_ever_active() {
        let mut store = CommandStore::default();

        // A pane that exited without a recorded code (e.g. killed by signal)
        // → Done (not Error), with on_focus=Some(Idle) so it clears when focused.
        store.on_exit(1, None, 5);
        let s = store
            .get(1)
            .expect("must have a resolved entry after on_exit(None)");
        assert_eq!(s.status, Status::Done, "None exit_status must yield Done");
        assert_eq!(
            s.on_focus,
            Some(Status::Idle),
            "on_focus must be set to Idle"
        );
        // A fast `zellij run -- false` that never reached Running must still
        // render as active (✗), so ever_active must be true even for a pane
        // with no prior resolved entry.
        assert!(
            s.ever_active,
            "ever_active must be true for a pane with no prior resolved entry"
        );
    }

    #[test]
    fn on_exit_preserves_existing_repo_and_msg() {
        let mut store = CommandStore::default();
        // Set up Running state
        store.on_command_changed(
            1,
            &["cargo".to_string(), "test".to_string()],
            true,
            Some("/work/pinky"),
            1,
        );
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);
        assert_eq!(store.get(1).unwrap().repo, "pinky");
        assert_eq!(store.get(1).unwrap().msg, "cargo test");
        assert_eq!(store.get(1).unwrap().kind, Kind::Test);

        // Exit 0 → Done, but repo and msg preserved
        store.on_exit(1, Some(0), 3);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.repo, "pinky", "repo must be preserved");
        assert_eq!(s.msg, "cargo test", "msg must be preserved");
    }

    // ── A: agent binaries are push-tracked, never command-tracked ──

    #[test]
    fn agent_foreground_commands_are_not_tracked() {
        // Push-instrumented agents report their status via the push pipe. Their
        // foreground command must leave NO command-store trace — otherwise
        // Zellij's CommandChanged churn (agent → tool subprocess → agent)
        // flickers the row between Running and Done and rewrites its message.
        // The set of suppressed agents is exactly the push adapters (see the
        // `agent_names_match_push_adapter_sources` guard); Gemini is NOT one —
        // see `gemini_foreground_command_is_tracked`.
        for agent in &["claude", "codex"] {
            let mut store = CommandStore::default();
            store.on_command_changed(1, &[agent.to_string()], true, Some("/work/repo"), 1);
            assert!(
                !store.pending.contains_key(&1),
                "{agent} must not enter pending"
            );
            store.on_timer(2);
            assert!(
                store.get(1).is_none(),
                "{agent} must leave no resolved command state"
            );
        }
    }

    #[test]
    fn gemini_foreground_command_is_tracked() {
        // Gemini has no push adapter (the shipped scope is Claude + Codex), so
        // unlike them it is *observed* via command-tracking rather than
        // suppressed — otherwise its panes would show nothing at all. It carries
        // its own `Kind::Gemini` source so it renders with the gemini mark.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &["gemini".to_string()], true, Some("/work/repo"), 1);
        store.on_timer(2);
        let s = store
            .get(1)
            .expect("gemini must leave a resolved command observation");
        assert_eq!(s.status, Status::Running);
        assert_eq!(s.kind, Kind::Gemini);
    }

    // ── B: leaving the foreground is debounced before flipping to Done ──

    #[test]
    fn leaving_foreground_debounces_before_marking_done() {
        let mut store = CommandStore::default();
        store.on_command_changed(1, &["make".to_string()], true, Some("/repo"), 1);
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        // Return-to-shell: tentative — must still read Running this instant.
        store.on_command_changed(1, &[], false, None, 3);
        assert_eq!(
            store.get(1).unwrap().status,
            Status::Running,
            "leaving the foreground must not flip to Done instantly"
        );

        // Timer past the debounce window → now Done.
        store.on_timer(4);
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.on_focus, Some(Status::Idle));
        assert_eq!(s.last_change_tick, 4);
    }

    #[test]
    fn brief_foreground_drop_replaced_by_command_never_shows_done() {
        // A pane that briefly drops out of the foreground then immediately runs
        // another real command (e.g. a wrapper spawning a child) must never show
        // a spurious Done in between.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &["make".to_string()], true, Some("/repo"), 1);
        store.on_timer(2);
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        store.on_command_changed(1, &[], false, None, 3);
        store.on_command_changed(1, &["rg".to_string(), "needle".to_string()], true, Some("/repo"), 3);

        store.on_timer(4);
        assert_eq!(
            store.get(1).unwrap().status,
            Status::Running,
            "a brief fg drop replaced by a new command must never surface Done"
        );
    }

    // ── C: an empty/unknown foreground command never becomes a blank row ──

    #[test]
    fn empty_foreground_command_is_never_promoted() {
        let mut store = CommandStore::default();
        store.on_command_changed(1, &[], true, Some("/repo"), 1);
        assert!(
            !store.pending.contains_key(&1),
            "empty fg argv must not enter pending"
        );
        store.on_timer(2);
        assert!(
            store.get(1).is_none(),
            "empty fg command must leave no resolved state (no blank Running row)"
        );
    }

    #[test]
    fn on_pane_focused_same_status_does_not_update_tick() {
        let mut store = CommandStore::default();
        // Place pane in Done with on_focus=Some(Done) (same status → no tick update)
        store.on_exit(1, Some(0), 5);
        // Manually set on_focus to Done (same as current status) to test tick stability
        store.resolved.get_mut(&1).unwrap().on_focus = Some(Status::Done);
        store.on_pane_focused(1, 10);
        assert_eq!(store.get(1).unwrap().status, Status::Done);
        // last_change_tick should NOT be updated (status did not change)
        assert_eq!(store.get(1).unwrap().last_change_tick, 5);
    }
