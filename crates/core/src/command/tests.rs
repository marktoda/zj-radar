    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| part.to_string()).collect()
    }

    #[test]
    fn is_shell_prompt_detects_return_to_prompt_not_agents_or_commands() {
        // A shell/prompt program in the foreground = back at the prompt.
        assert!(is_shell_prompt(&argv(&["zsh"]), true));
        assert!(is_shell_prompt(&argv(&["/bin/bash"]), true));
        assert!(is_shell_prompt(&argv(&["fish"]), true));
        // No foreground command at all = at the prompt.
        assert!(is_shell_prompt(&argv(&["anything"]), false));
        // An agent in the foreground still owns the pane — NOT the prompt.
        assert!(!is_shell_prompt(&argv(&["claude"]), true));
        assert!(!is_shell_prompt(&argv(&["codex"]), true));
        // A real foreground command is not the prompt.
        assert!(!is_shell_prompt(&argv(&["cargo", "test"]), true));
        // Env/wrapper prefixes are peeled before classifying.
        assert!(is_shell_prompt(&argv(&["env", "FOO=1", "zsh"]), true));
        assert!(!is_shell_prompt(&argv(&["sudo", "make"]), true));
    }

    #[test]
    fn is_shell_prompt_covers_non_posix_shells_and_login_argv0() {
        // Regression: missing shells here made the shell itself track as a
        // perpetual Running command AND broke the agent exit-clear (the two
        // degradations documented on IGNORE_NAMES).
        for shell in ["nu", "nushell", "pwsh", "tcsh", "csh", "ksh", "mksh", "ash", "elvish", "xonsh"] {
            assert!(is_shell_prompt(&argv(&[shell]), true), "{shell} is a prompt");
        }
        // A login shell's argv0 carries a leading dash.
        assert!(is_shell_prompt(&argv(&["-zsh"]), true));
        assert!(is_shell_prompt(&argv(&["-bash"]), true));
        // The dash-strip must not misread ordinary dashed commands as shells:
        // there is no binary named e.g. `-nu` in practice, but a real command
        // with a dashed basename stays a command.
        assert!(!is_shell_prompt(&argv(&["my-tool"]), true));
    }

    #[test]
    fn is_agent_foreground_detects_only_a_live_foreground_agent() {
        // The agent's own exe in the foreground vouches for the pushed status.
        assert!(is_agent_foreground(&argv(&["claude"]), true));
        assert!(is_agent_foreground(&argv(&["codex"]), true));
        // Env/wrapper prefixes are peeled, mirroring is_shell_prompt.
        assert!(is_agent_foreground(&argv(&["env", "FOO=1", "claude"]), true));
        // Background, shells, ordinary commands, and nothing don't vouch.
        assert!(!is_agent_foreground(&argv(&["claude"]), false));
        assert!(!is_agent_foreground(&argv(&["zsh"]), true));
        assert!(!is_agent_foreground(&argv(&["vim"]), true));
        assert!(!is_agent_foreground(&argv(&[]), true));
    }

    /// Test shorthand for the display half of `classify`.
    fn display(command: &[String]) -> String {
        classify(command).0
    }

    #[test]
    fn classify_display_keeps_useful_subcommands_and_drops_flags() {
        assert_eq!(
            display(&argv(&[
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
            display(&argv(&[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5"
            ])),
            "codex"
        );
        assert_eq!(
            display(&argv(&["npm", "run", "build", "--", "--watch"])),
            "npm run build"
        );
        assert_eq!(
            display(&argv(&["python", "-m", "pytest", "-q", "tests/render.rs"])),
            "python -m pytest tests/render.rs"
        );
        assert_eq!(display(&argv(&["sleep", "5"])), "sleep 5");
    }

    /// Pin the display half of `classify` across every branch of the table-driven
    /// model: each `ToolRule` shape (`subcommands: Some` known-sub lookup vs
    /// `None` first-arg), in/out of `target_verbs`, the `FIRST_ARG_RULE`
    /// fallback (proving the former pytest|ruff|make|just arm still resolves
    /// natively), the `python -m` path, agents, and the unknown/bare-exe
    /// collapse. One case per code path so the refactor's equivalence is
    /// verifiable, not merely asserted.
    #[test]
    fn classify_display_covers_every_tool_rule_path() {
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
            // Versioned interpreter basenames route to the python shape too —
            // `python3.12 app.py` must not fall to FIRST_ARG_RULE and show the
            // un-basenamed script path.
            (&["python3.12", "-m", "pytest", "-q", "t.py"], "python3.12 -m pytest t.py"),
            (&["python3.12", "path/to/app.py"], "python3.12 app.py"),
            (&["python2", "path/to/app.py"], "python2 app.py"),
            (&["/usr/bin/python3.12", "serve.py"], "python3.12 serve.py"),
            // …but a python-*tool* is not an interpreter: only a digits/dots
            // suffix counts, so `python-config` keeps the first-arg treatment
            // (its arg is NOT basenamed like a script would be).
            (&["python-config", "a/b"], "python-config a/b"),
            // Deliberate post-refactor behavior: the uniform target dash-guard
            // drops a bare "-" target (old cargo nextest / go test kept it).
            (&["go", "test", "-"], "go test"),
            (&["cargo", "nextest", "-"], "cargo nextest"),
        ];
        for (args, want) in cases {
            assert_eq!(&display(&argv(args)), want, "display for {args:?}");
        }
    }

    /// Representative `(argv, expected display, expected Kind)` covering every
    /// `Kind` that `classify` can emit. Shared by the classification test and
    /// the Kind-round-trip guard so both exercise exactly the same set. The
    /// display column is ASSERTED, not passed in — `classify` owns the
    /// display↔kind pairing, so a hand-written display here can no longer
    /// silently diverge from what production computes.
    fn kind_classification_cases() -> Vec<(Vec<String>, &'static str, Kind)> {
        use crate::kind::Kind;
        vec![
            // Agents, by basename.
            (argv(&["claude"]), "claude", Kind::Claude),
            (argv(&["codex", "--dangerously-bypass-sandbox"]), "codex", Kind::Codex),
            (argv(&["gemini"]), "gemini", Kind::Gemini),
            // Test runners across ecosystems.
            // The `--features` VALUE reads as a target ("cargo test cli") — a
            // pre-existing display wart (value-taking flags aren't modeled),
            // surfaced the moment this column became asserted instead of
            // hand-written. Pinned as-is; the Kind is what this case is about.
            (argv(&["cargo", "test", "--features", "cli"]), "cargo test cli", Kind::Test),
            (argv(&["pytest"]), "pytest", Kind::Test),
            // `python -m pytest` routes through the python display path but
            // must classify like bare pytest (PYTHON_RULE's word scan), not
            // fall to FIRST_ARG_RULE's empty kind columns.
            (argv(&["python", "-m", "pytest", "-q", "t.py"]), "python -m pytest t.py", Kind::Test),
            (argv(&["go", "test", "./..."]), "go test ./...", Kind::Test),
            (argv(&["npm", "run", "test"]), "npm run test", Kind::Test),
            // Build.
            (argv(&["cargo", "build"]), "cargo build", Kind::Build),
            (argv(&["npm", "run", "build"]), "npm run build", Kind::Build),
            // Server and deploy (npm dev-server; make/just verb routing).
            (argv(&["npm", "run", "dev"]), "npm run dev", Kind::Server),
            (argv(&["just", "serve"]), "just serve", Kind::Server),
            (argv(&["make", "deploy"]), "make deploy", Kind::Deploy),
            // Closed vs open verb vocabularies: cargo classifies by its known
            // subcommand only, so a *target* named test-server is not a test —
            // while npm scripts are the user's own words, so the word-bounded
            // scan reads test-watch as a test.
            (argv(&["cargo", "run", "test-server"]), "cargo run test-server", Kind::Command),
            (argv(&["npm", "run", "test-watch"]), "npm run test-watch", Kind::Test),
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
            store.on_timer(Tick(1 + DEBOUNCE_TICKS), EpochSecs(0));
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
        store.on_timer(Tick(2), EpochSecs(0));
        assert!(store.get(1).is_none(), "wrapped agent must stay suppressed");
    }

    #[test]
    fn unknown_wrapper_options_are_left_alone() {
        // `sudo -u user make` carries a value-taking option we don't model;
        // rather than mis-parse it, peeling bails and leaves the command as-is
        // (no regression vs. not peeling). It still tracks as a generic command.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &argv(&["sudo", "-u", "user", "make"]), true, Some("/r"), 1);
        store.on_timer(Tick(1 + DEBOUNCE_TICKS), EpochSecs(0));
        let s = store.get(1).expect("should still be tracked");
        assert_eq!(s.kind, Kind::Command);
    }

    #[test]
    fn classify_emits_every_kind_with_its_paired_display() {
        for (cmd, want_display, want_kind) in kind_classification_cases() {
            let (display, kind) = classify(&cmd);
            assert_eq!(kind, want_kind, "kind for {want_display:?}");
            assert_eq!(display, want_display, "display for {cmd:?}");
        }
    }

    #[test]
    fn classify_is_case_insensitive_over_verbs() {
        // `contains_word` requires a lowercased haystack; classify owns the
        // lowering, so uppercase targets/scripts must still classify.
        let cases: &[(&[&str], Kind)] = &[
            (&["make", "TEST"], Kind::Test),
            (&["npm", "run", "BUILD"], Kind::Build),
            (&["just", "Serve"], Kind::Server),
            (&["make", "Deploy-Prod"], Kind::Deploy),
        ];
        for (cmd, expected) in cases {
            assert_eq!(classify(&argv(cmd)).1, *expected, "classify {cmd:?}");
        }
    }

    #[test]
    fn contains_word_matches_whole_words_only() {
        // The shared lexical primitive (core home for the rule the bash producer
        // and the CLI agent adapters also ride). Matches on non-`[a-z0-9]`
        // boundaries and string edges; a keyword inside a larger token does not.
        assert!(contains_word("make test", "test"));
        assert!(contains_word("test", "test")); // both edges
        assert!(contains_word("build-all", "build")); // `-` is a boundary
        assert!(contains_word("run_test", "test")); // `_` is a boundary
        assert!(contains_word("git push origin", "git push")); // phrase; inner space is a boundary
        assert!(!contains_word("latest", "test")); // embedded → no match
        assert!(!contains_word("rebuild", "build"));
        assert!(!contains_word("tests", "test")); // trailing `s` is alphanumeric
        assert!(!contains_word("observer", "serve"));
        assert!(!contains_word("", "test")); // empty haystack
    }

    #[test]
    fn make_and_npm_classification_is_word_bounded() {
        use crate::kind::Kind;
        // A keyword embedded in a larger target token must NOT classify: these
        // are the false positives an unanchored `contains` produced.
        let plain: &[&[&str]] = &[
            &["make", "latest"],     // not `test`
            &["make", "rebuild"],    // not `build`
            &["make", "observer"],   // not `serve`/`server`
            &["just", "codev"],      // not `dev`
            &["npm", "run", "latest"],
        ];
        for argv_ in plain {
            assert_eq!(classify(&argv(argv_)).1, Kind::Command, "classify {argv_:?}");
        }
        // A keyword as a whole token — including across `-`/`_` boundaries —
        // still classifies.
        let routed: &[(&[&str], Kind)] = &[
            (&["make", "build-all"], Kind::Build),
            (&["make", "deploy-prod"], Kind::Deploy),
            (&["just", "unit-test"], Kind::Test),
            (&["make", "server"], Kind::Server),
        ];
        for (argv_, want) in routed {
            assert_eq!(classify(&argv(argv_)).1, *want, "classify {argv_:?}");
        }
    }

    #[test]
    fn command_source_round_trips_through_kind() {
        // Twin of the agent-side `source_round_trips_through_kind` (see
        // CONTEXT.md "Information source"). The command path stores
        // `classify(..).1.as_source()` and the roll-up reads it back via
        // `Kind::from_source`, so every classified command must survive that
        // round-trip to the SAME kind — and never degrade to `Kind::Other`,
        // the reserved sentinel for a genuinely-unknown source. (Kind's own
        // universal round-trip is guarded in `kind.rs`; this pins that the
        // command boundary actually rides that seam.)
        use crate::kind::Kind;
        for (cmd, display, _) in kind_classification_cases() {
            let (_, kind) = classify(&cmd);
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
        // `on_command_changed` → `on_timer`, not only `classify` in
        // isolation.
        use crate::kind::Kind;
        let mut store = CommandStore::default();
        let cmd = argv(&["cargo", "test", "--features", "cli"]);
        store.on_command_changed(1, &cmd, true, Some("/home/u/repo"), 1);
        store.on_timer(Tick(1 + DEBOUNCE_TICKS), EpochSecs(0));
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

        // t=1: timer fires at same tick → not past debounce (0 < DEBOUNCE_TICKS)
        store.on_timer(Tick(1), EpochSecs(0));
        assert!(store.get(1).is_none(), "still pending at same tick");

        // One tick short of the debounce window → still pending. Only exercises
        // something when the floor is above 1.
        if DEBOUNCE_TICKS > 1 {
            store.on_timer(Tick(DEBOUNCE_TICKS), EpochSecs(0));
            assert!(store.get(1).is_none(), "still pending one tick short of debounce");
        }

        // t = 1 + DEBOUNCE_TICKS: timer fires past debounce → promote
        store.on_timer(Tick(1 + DEBOUNCE_TICKS), EpochSecs(0));
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

    #[test]
    fn on_timer_reports_observation_changes_for_snapshot_persistence() {
        // The return value drives snapshot persistence in the plugin runtime:
        // true exactly when an observation mutated (promotion or Done-flip),
        // false on a quiet tick — so late-spawned instances converge without
        // per-tick snapshot churn.
        let mut store = CommandStore::default();
        let cmd = vec!["cargo".to_string(), "test".to_string()];

        assert!(!store.on_timer(Tick(1), EpochSecs(0)).changed, "empty store: quiet tick");

        store.on_command_changed(1, &cmd, true, Some("/w/repo"), 1);
        let promote_tick = 1 + DEBOUNCE_TICKS;
        assert!(store.on_timer(Tick(promote_tick), EpochSecs(0)).changed, "debounced promotion mutates the store");
        assert!(!store.on_timer(Tick(promote_tick + 1), EpochSecs(0)).changed, "already Running: quiet tick");

        let leave_tick = promote_tick + 2;
        store.on_command_changed(1, &[], false, None, leave_tick); // leaves foreground
        let done_tick = leave_tick + DEBOUNCE_TICKS;
        assert!(store.on_timer(Tick(done_tick), EpochSecs(0)).changed, "confirmed Done-flip mutates the store");
        assert!(!store.on_timer(Tick(done_tick + 1), EpochSecs(0)).changed, "terminal Done: quiet tick");
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
        store.on_timer(Tick(5), EpochSecs(0));
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

    // direnv's per-prompt `direnv export <shell>` hook is back-to-the-prompt
    // machinery (like starship), not a tracked command: it must neither open its
    // own lifecycle nor clobber the real command that just finished on the pane.
    #[test]
    fn direnv_export_hook_is_ignored_like_a_prompt() {
        let mut store = CommandStore::default();

        // A real command runs and is promoted to Running.
        store.on_command_changed(1, &argv(&["cargo", "test"]), true, Some("/repo"), 1);
        let promote_tick = 1 + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote_tick), EpochSecs(0));
        assert_eq!(store.get(1).unwrap().status, Status::Running);
        assert_eq!(store.get(1).unwrap().msg, "cargo test");

        // The shell prompt hook fires `direnv export zsh` as the next foreground
        // command. Being prompt machinery, it must confirm the prior command's
        // completion — NOT open a fresh "direnv" lifecycle that clobbers it.
        let direnv_tick = promote_tick + 1;
        store.on_command_changed(1, &argv(&["direnv", "export", "zsh"]), true, Some("/repo"), direnv_tick);
        assert!(
            !store.pending.contains_key(&1),
            "direnv export must not open a new command lifecycle"
        );

        // Debounce → Done, still identified as the cargo test, not direnv.
        store.on_timer(Tick(direnv_tick + DEBOUNCE_TICKS), EpochSecs(0));
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.msg, "cargo test", "the finished command is cargo test, not direnv");
    }

    // ── Test 4: Running → return-to-shell → Done

    #[test]
    fn running_to_return_to_shell_sets_done() {
        let mut store = CommandStore::default();
        let cmd = vec!["make".to_string()];

        // t=1: fg real command
        store.on_command_changed(1, &cmd, true, Some("/repo"), 1);
        // promote to Running after the debounce floor
        let promote_tick = 1 + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote_tick), EpochSecs(0));
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        // return-to-shell (is_foreground=false) → tentative, still Running
        let leave_tick = promote_tick + 1;
        store.on_command_changed(1, &[], false, None, leave_tick);
        assert_eq!(store.get(1).unwrap().status, Status::Running);
        // timer past debounce → Done
        let done_tick = leave_tick + DEBOUNCE_TICKS;
        store.on_timer(Tick(done_tick), EpochSecs(0));
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.last_change_tick, done_tick);
    }

    // ── Test 5: on_exit(Some(0)) → Done; on_exit(Some(3)) → Error; dedupe

    #[test]
    fn on_exit_sets_status_and_dedupes() {
        let mut store = CommandStore::default();

        // Exit 0 → Done
        store.on_exit(1, Some(0), Tick(5), EpochSecs(0));
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);

        // Repeated identical exit → no-op (tick unchanged)
        store.on_exit(1, Some(0), Tick(10), EpochSecs(0));
        let s = store.get(1).unwrap();
        assert_eq!(
            s.last_change_tick, 5,
            "repeated identical exit must be a no-op"
        );

        // Pane 2: nonzero exit → Error
        store.on_exit(2, Some(3), Tick(6), EpochSecs(0));
        let s = store.get(2).unwrap();
        assert_eq!(s.status, Status::Error);

        // Repeated identical exit for pane 2 → no-op
        store.on_exit(2, Some(3), Tick(99), EpochSecs(0));
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
        let promote1 = 1 + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote1), EpochSecs(0));
        assert_eq!(store.get(7).unwrap().status, Status::Running);
        store.on_exit(7, Some(0), Tick(promote1 + 1), EpochSecs(0));
        assert_eq!(store.get(7).unwrap().status, Status::Done);

        // Re-run in the same (still-live) pane: back to Running.
        let rerun_tick = promote1 + 2;
        store.on_command_changed(7, &argv(&["sleep", "5"]), true, Some("/r"), rerun_tick);
        let promote2 = rerun_tick + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote2), EpochSecs(0));
        assert_eq!(store.get(7).unwrap().status, Status::Running);

        // Second run exits with the SAME code — must resolve to Done, not stay Running.
        store.on_exit(7, Some(0), Tick(promote2 + 1), EpochSecs(0));
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
        store.on_timer(Tick(1 + DEBOUNCE_TICKS), EpochSecs(0));
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
        store.on_timer(Tick(1 + DEBOUNCE_TICKS), EpochSecs(0));
        // Set up pane 3: has exit record
        store.on_exit(3, Some(0), Tick(1), EpochSecs(0));

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
        let promote_tick = 1 + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote_tick), EpochSecs(0));
        assert!(store.has_pending_or_active(), "true while Running");

        // Return to shell → tentative; still active (Running) until debounce.
        let leave_tick = promote_tick + 1;
        store.on_command_changed(1, &[], false, None, leave_tick);
        assert!(
            store.has_pending_or_active(),
            "still active until the debounce window flips it to Done"
        );

        // Timer past debounce → Done (no pending, no Running).
        store.on_timer(Tick(leave_tick + DEBOUNCE_TICKS), EpochSecs(0));
        assert!(
            !store.has_pending_or_active(),
            "false once Done (no pending, no Running)"
        );
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

    // ── Test: on_exit(None) → Done, ever_active=true

    #[test]
    fn on_exit_none_yields_done_and_ever_active() {
        let mut store = CommandStore::default();

        // A pane that exited without a recorded code (e.g. killed by signal)
        // → Done (not Error).
        store.on_exit(1, None, Tick(5), EpochSecs(0));
        let s = store
            .get(1)
            .expect("must have a resolved entry after on_exit(None)");
        assert_eq!(s.status, Status::Done, "None exit_status must yield Done");
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
        let promote_tick = 1 + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote_tick), EpochSecs(0));
        assert_eq!(store.get(1).unwrap().status, Status::Running);
        assert_eq!(store.get(1).unwrap().repo, "pinky");
        assert_eq!(store.get(1).unwrap().msg, "cargo test");
        assert_eq!(store.get(1).unwrap().kind, Kind::Test);

        // Exit 0 → Done, but repo and msg preserved
        store.on_exit(1, Some(0), Tick(promote_tick + 1), EpochSecs(0));
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
            store.on_timer(Tick(2), EpochSecs(0));
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
        store.on_timer(Tick(1 + DEBOUNCE_TICKS), EpochSecs(0));
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
        let promote_tick = 1 + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote_tick), EpochSecs(0));
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        // Return-to-shell: tentative — must still read Running this instant.
        let leave_tick = promote_tick + 1;
        store.on_command_changed(1, &[], false, None, leave_tick);
        assert_eq!(
            store.get(1).unwrap().status,
            Status::Running,
            "leaving the foreground must not flip to Done instantly"
        );

        // Timer past the debounce window → now Done.
        let done_tick = leave_tick + DEBOUNCE_TICKS;
        store.on_timer(Tick(done_tick), EpochSecs(0));
        let s = store.get(1).unwrap();
        assert_eq!(s.status, Status::Done);
        assert_eq!(s.last_change_tick, done_tick);
    }

    #[test]
    fn brief_foreground_drop_replaced_by_command_never_shows_done() {
        // A pane that briefly drops out of the foreground then immediately runs
        // another real command (e.g. a wrapper spawning a child) must never show
        // a spurious Done in between.
        let mut store = CommandStore::default();
        store.on_command_changed(1, &["make".to_string()], true, Some("/repo"), 1);
        let promote_tick = 1 + DEBOUNCE_TICKS;
        store.on_timer(Tick(promote_tick), EpochSecs(0));
        assert_eq!(store.get(1).unwrap().status, Status::Running);

        let blip_tick = promote_tick + 1;
        store.on_command_changed(1, &[], false, None, blip_tick);
        store.on_command_changed(1, &["rg".to_string(), "needle".to_string()], true, Some("/repo"), blip_tick);

        store.on_timer(Tick(blip_tick + DEBOUNCE_TICKS), EpochSecs(0));
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
        store.on_timer(Tick(2), EpochSecs(0));
        assert!(
            store.get(1).is_none(),
            "empty fg command must leave no resolved state (no blank Running row)"
        );
    }

    // ── Done TTL recede, epoch stamping, easing-safe promotion ──

    #[test]
    fn done_recedes_to_idle_after_ttl_and_reports_the_recede() {
        let mut s = CommandStore::default();
        s.on_command_changed(1, &argv(&["cargo", "build"]), true, None, 0);
        s.on_timer(Tick(DEBOUNCE_TICKS), EpochSecs(100));                       // promote at debounce
        s.on_command_changed(1, &argv(&["zsh"]), true, None, 3); // back to prompt
        let done_tick = DEBOUNCE_TICKS + 3;
        s.on_timer(Tick(done_tick), EpochSecs(200));                            // confirm Done
        assert_eq!(s.get(1).unwrap().status, Status::Done);
        assert_eq!(s.get(1).unwrap().completed_epoch_s, Some(200), "stamped at completion");

        let before = s.on_timer(Tick(done_tick + DONE_TTL_TICKS - 1), EpochSecs(300));
        assert!(before.receded.is_empty(), "still inside the TTL window");
        assert_eq!(s.get(1).unwrap().status, Status::Done);

        let after = s.on_timer(Tick(done_tick + DONE_TTL_TICKS), EpochSecs(301));
        assert_eq!(s.get(1).unwrap().status, Status::Idle, "receded");
        assert!(s.get(1).unwrap().ever_active, "idle row stays a muted row, not removed");
        assert!(after.changed);
        assert_eq!(after.receded.len(), 1);
        assert_eq!(after.receded[0].1.status, Status::Done, "the receded obs is the completion");
        assert_eq!(after.receded[0].1.completed_epoch_s, Some(200), "original stamp rides along");
    }

    #[test]
    fn error_is_exempt_from_ttl_but_counts_nothing_toward_arming() {
        let mut s = CommandStore::default();
        s.on_exit(1, Some(2), Tick(5), EpochSecs(100));
        let r = s.on_timer(Tick(5 + DONE_TTL_TICKS + 10), EpochSecs(200));
        assert_eq!(s.get(1).unwrap().status, Status::Error, "errors persist");
        assert!(r.receded.is_empty());
        assert!(!s.has_done_awaiting_recede(), "Error must not pin the timer");
    }

    #[test]
    fn done_awaiting_recede_arms_until_ttl_fires() {
        let mut s = CommandStore::default();
        s.on_exit(1, Some(0), Tick(5), EpochSecs(100));
        assert!(s.has_done_awaiting_recede());
        s.on_timer(Tick(5 + DONE_TTL_TICKS), EpochSecs(200));
        assert!(!s.has_done_awaiting_recede());
    }

    #[test]
    fn promotion_preserves_running_since_for_same_command() {
        // Zellij re-reporting a still-foreground command must not reset the
        // "entered Running" tick, or a long-runner never eases (spec §8).
        let mut s = CommandStore::default();
        s.on_command_changed(1, &argv(&["cargo", "build"]), true, None, 0);
        s.on_timer(Tick(DEBOUNCE_TICKS), EpochSecs(100));
        let t0 = s.get(1).unwrap().last_change_tick;
        s.on_command_changed(1, &argv(&["cargo", "build"]), true, None, 50); // re-report
        s.on_timer(Tick(50 + DEBOUNCE_TICKS), EpochSecs(150));                                // re-promote
        assert_eq!(s.get(1).unwrap().last_change_tick, t0, "same command keeps its start tick");
    }

    #[test]
    fn promotion_over_a_done_reports_the_displaced_completion() {
        let mut s = CommandStore::default();
        s.on_exit(1, Some(0), Tick(5), EpochSecs(100)); // Done sitting on the pane
        s.on_command_changed(1, &argv(&["make"]), true, None, 6);
        let r = s.on_timer(Tick(6 + DEBOUNCE_TICKS), EpochSecs(200));
        assert_eq!(r.receded.len(), 1, "the old Done left the card via overwrite");
        assert_eq!(r.receded[0].1.status, Status::Done);
        assert_eq!(s.get(1).unwrap().status, Status::Running);
    }

    #[test]
    fn bare_exit_replay_after_recede_never_resurrects() {
        // Level-triggered exits: every PaneUpdate re-delivers a held pane's exit.
        // After the TTL recede, those replays must be inert — no Done flap, no
        // fresh completion stamp.
        let mut s = CommandStore::default();
        s.on_exit(9, Some(0), Tick(5), EpochSecs(100));
        s.on_timer(Tick(5 + DONE_TTL_TICKS), EpochSecs(200)); // recede
        assert_eq!(s.get(9).unwrap().status, Status::Idle);
        s.on_exit(9, Some(0), Tick(5 + DONE_TTL_TICKS + 1), EpochSecs(300)); // manifest replay
        assert_eq!(s.get(9).unwrap().status, Status::Idle, "no resurrection");
        assert_eq!(s.get(9).unwrap().completed_epoch_s, None, "no fresh stamp");
    }

    #[test]
    fn fresh_instance_exit_replay_of_displayed_completion_is_inert() {
        // A freshly-spawned instance (empty `exited` dedup) receives the
        // level-triggered exit replay for a held pane whose completion it
        // already shows from the loaded snapshot. That replay is not news:
        // it must not push the still-displayed completion into `receded`
        // (a ghost ledger entry — the card never changed), must not re-stamp
        // `completed_epoch_s = now` (a duplicate surviving the nearest-
        // neighbor merge once the delta exceeds MERGE_WINDOW_S), and must not
        // bump `last_change_tick` (bumping would postpone the Done TTL
        // forever under repeated replays).
        for (code, status) in [(Some(0), Status::Done), (Some(2), Status::Error)] {
            let mut s = CommandStore::default();
            let obs = TrackedObservation {
                exit_code: code,
                completed_epoch_s: Some(100),
                ..TrackedObservation::command(status, "repo".into(), "make".into(), Kind::Command, 5)
            };
            s.insert_snapshot_observation(9, obs);

            let receded = s.on_exit(9, code, Tick(50), EpochSecs(999));
            assert!(receded.is_none(), "{status:?}: an identical replay must not ghost-ledger");
            let got = s.get(9).unwrap();
            assert_eq!(got.status, status, "{status:?}: unchanged");
            assert_eq!(got.completed_epoch_s, Some(100), "{status:?}: original stamp survives");
            assert_eq!(got.last_change_tick, 5, "{status:?}: no tick bump — the TTL clock must keep running");

            // The dedup map is primed: a second identical replay no-ops too.
            let receded = s.on_exit(9, code, Tick(51), EpochSecs(1000));
            assert!(receded.is_none(), "{status:?}: dedup primed after the first swallow");
            assert_eq!(s.get(9).unwrap().completed_epoch_s, Some(100));
        }
    }

    #[test]
    fn different_exit_code_on_displayed_completion_still_displaces() {
        // Counter-case: a DIFFERENT exit code against a non-pending completion
        // is a genuine new outcome (a held run-pane re-run whose fresh
        // `CommandChanged` was missed or hasn't landed) — it must displace the
        // old completion (receding it to the ledger) and stamp the new one.
        let mut s = CommandStore::default();
        let obs = TrackedObservation {
            exit_code: Some(0),
            completed_epoch_s: Some(100),
            ..TrackedObservation::command(Status::Done, "repo".into(), "make".into(), Kind::Command, 5)
        };
        s.insert_snapshot_observation(9, obs);

        let receded = s.on_exit(9, Some(2), Tick(50), EpochSecs(999))
            .expect("the displayed Done leaves via displacement");
        assert_eq!(receded.status, Status::Done);
        assert_eq!(receded.completed_epoch_s, Some(100));
        let got = s.get(9).unwrap();
        assert_eq!(got.status, Status::Error);
        assert_eq!(got.exit_code, Some(2));
        assert_eq!(got.completed_epoch_s, Some(999), "the new outcome stamps fresh");
    }

    #[test]
    fn exit_before_promotion_wears_the_pending_commands_label() {
        // A run-pane command can exit inside the debounce window (fast command,
        // or the manifest exit landing first) — before promotion ever inserted
        // an observation. The completion must wear the pending run's identity
        // (command, kind, cwd-derived repo — exactly what promotion would have
        // stamped), never a blank Kind::Command row.
        let mut s = CommandStore::default();
        s.on_command_changed(1, &argv(&["cargo", "build"]), true, Some("/work/myrepo"), 1);
        s.on_exit(1, Some(1), Tick(2), EpochSecs(100)); // beats the debounce promotion
        let got = s.get(1).unwrap();
        assert_eq!(got.status, Status::Error);
        assert_eq!(got.msg, "cargo build", "labeled with the run that exited");
        assert_eq!(got.kind, Kind::Build);
        assert_eq!(got.repo, "myrepo", "repo derived from the pending cwd, like promotion");
        assert_eq!(got.exit_code, Some(1));
        assert_eq!(got.completed_epoch_s, Some(100));
    }

    #[test]
    fn rerun_exit_before_promotion_does_not_wear_the_previous_runs_label() {
        // First run finishes as `cargo test` (Done). A re-run of `cargo build`
        // in the same held pane exits before its debounce promotion: the
        // failure must read "cargo build", not resurrect "cargo test" — and
        // the displaced Done still hands off to the ledger under ITS label.
        let mut s = CommandStore::default();
        s.on_command_changed(1, &argv(&["cargo", "test"]), true, Some("/work/myrepo"), 1);
        let promote = 1 + DEBOUNCE_TICKS;
        s.on_timer(Tick(promote), EpochSecs(100));
        s.on_exit(1, Some(0), Tick(promote + 1), EpochSecs(200));
        assert_eq!(s.get(1).unwrap().status, Status::Done);
        assert_eq!(s.get(1).unwrap().msg, "cargo test");

        let rerun = promote + 2;
        s.on_command_changed(1, &argv(&["cargo", "build"]), true, Some("/work/myrepo"), rerun);
        let receded = s.on_exit(1, Some(101), Tick(rerun + 1), EpochSecs(300)).expect("the old Done is displaced");
        assert_eq!(receded.status, Status::Done);
        assert_eq!(receded.msg, "cargo test", "the ledgered completion is the OLD run");
        let got = s.get(1).unwrap();
        assert_eq!(got.status, Status::Error);
        assert_eq!(got.msg, "cargo build", "the failure wears the NEW run's label");
        assert_eq!(got.kind, Kind::Build);
        assert_eq!(got.completed_epoch_s, Some(300));
    }

    #[test]
    fn rerun_with_command_changed_after_recede_applies_its_exit() {
        // A genuine new lifecycle (CommandChanged → pending) re-lights the pane
        // even if its exit lands before the debounce promotion.
        let mut s = CommandStore::default();
        s.on_exit(9, Some(0), Tick(5), EpochSecs(100));
        s.on_timer(Tick(5 + DONE_TTL_TICKS), EpochSecs(200)); // recede
        let t = 5 + DONE_TTL_TICKS + 2;
        s.on_command_changed(9, &argv(&["make"]), true, None, t); // new run opens
        s.on_exit(9, Some(0), Tick(t + 1), EpochSecs(300));                        // exits pre-promotion
        assert_eq!(s.get(9).unwrap().status, Status::Done, "new run's completion applies");
        assert_eq!(s.get(9).unwrap().completed_epoch_s, Some(300));
    }

    // ── Debounce floor + the missed-exit-edge diagnosis ──

    #[test]
    fn sub_debounce_command_never_renders_running() {
        // Acceptance (spec §3.2): cd/ls-style instant commands never earn a line.
        let mut s = CommandStore::default();
        s.on_command_changed(1, &argv(&["cd"]), true, None, 0);
        s.on_command_changed(1, &argv(&["zsh"]), true, None, 0); // returns within the window
        let r = s.on_timer(Tick(DEBOUNCE_TICKS), EpochSecs(100));
        assert!(s.get(1).is_none(), "never promoted");
        assert!(!r.changed);
    }

    #[test]
    fn missed_exit_edge_is_the_stale_running_path() {
        // DIAGNOSIS pin: if the back-to-shell CommandChanged never arrives, the
        // pending promotes and the row sticks Running forever — this is the
        // `running cd` screenshot. The floor bump narrows the window to ~2s; a
        // stuck row beyond that implies a missing Zellij edge, not a store bug.
        let mut s = CommandStore::default();
        s.on_command_changed(1, &argv(&["cd"]), true, None, 0);
        // no follow-up event at all
        s.on_timer(Tick(DEBOUNCE_TICKS), EpochSecs(100));
        assert_eq!(s.get(1).unwrap().status, Status::Running, "documented failure mode");
    }

