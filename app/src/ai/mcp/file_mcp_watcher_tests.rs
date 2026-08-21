use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;

use futures::stream::AbortHandle;
use repo_metadata::repositories::RepoDetectionSource;
use repo_metadata::{RepositoryUpdate, TargetFile};
use settings::SettingsMode;
use warpui::App;

use super::{
    FileMCPConfigDiagnosticKind, FileMCPConfigParseOutcome, FileMCPScanOrigin, FileMCPWatcher,
    FileMCPWatcherEvent, InFlightParse, config_change_flags, home_subdir_to_watch,
    parse_mcp_config_file, providers_in_scope, should_watch_repository, substitute_env_vars,
};
use crate::ai::mcp::MCPProvider;
use crate::test_util::terminal::initialize_app_for_terminal_view;

/// Constructs a `FileMCPWatcher` singleton with an explicit initial-global-scan pending set,
/// bypassing the real home-directory scan in `FileMCPWatcher::new` so tests are deterministic
/// regardless of what actually exists on the test machine's filesystem.
fn setup_watcher_with_pending(
    app: &mut App,
    pending: HashSet<(PathBuf, MCPProvider)>,
) -> warpui::ModelHandle<FileMCPWatcher> {
    app.add_singleton_model(move |_ctx| FileMCPWatcher {
        file_mcp_tx: async_channel::unbounded().0,
        in_flight_parses: HashMap::new(),
        next_parse_generation: 0,
        home_provider_watchers: HashMap::new(),
        project_repo_watchers: HashSet::new(),
        cloud_env_pending: HashMap::new(),
        initial_global_scan_pending: pending,
        initial_global_scan_emitted: false,
    })
}

fn cleanup_env_vars(vars: &[&str]) {
    for var in vars {
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { env::remove_var(var) };
    }
}

#[test]
fn abort_config_parse_cancels_and_removes_inflight_task() {
    let (file_mcp_tx, _file_mcp_rx) = async_channel::unbounded();
    let config_path = PathBuf::from("/tmp/.mcp.json");
    let key = (config_path.clone(), MCPProvider::Warp);
    let (abort_handle, _abort_registration) = AbortHandle::new_pair();
    let observed_handle = abort_handle.clone();
    let mut watcher = FileMCPWatcher {
        file_mcp_tx,
        in_flight_parses: HashMap::from([(
            key.clone(),
            InFlightParse {
                generation: 0,
                abort_handle,
            },
        )]),
        next_parse_generation: 1,
        home_provider_watchers: HashMap::new(),
        project_repo_watchers: HashSet::new(),
        cloud_env_pending: HashMap::new(),
        initial_global_scan_pending: HashSet::new(),
        initial_global_scan_emitted: false,
    };

    watcher.abort_config_parse(&config_path, MCPProvider::Warp);

    assert!(observed_handle.is_aborted());
    assert!(!watcher.in_flight_parses.contains_key(&key));
}

#[test]
fn repository_discovery_is_surface_aware() {
    assert!(should_watch_repository(
        RepoDetectionSource::TerminalNavigation,
        SettingsMode::Gui
    ));
    assert!(should_watch_repository(
        RepoDetectionSource::CloudEnvironmentPrep,
        SettingsMode::Gui
    ));
    assert!(!should_watch_repository(
        RepoDetectionSource::ProjectRulesIndexing,
        SettingsMode::Gui
    ));
    assert!(!should_watch_repository(
        RepoDetectionSource::CodeReviewInitialization,
        SettingsMode::Gui
    ));

    assert!(should_watch_repository(
        RepoDetectionSource::TerminalNavigation,
        SettingsMode::Tui
    ));
    assert!(!should_watch_repository(
        RepoDetectionSource::ProjectRulesIndexing,
        SettingsMode::Tui
    ));
    assert!(!should_watch_repository(
        RepoDetectionSource::CodeReviewInitialization,
        SettingsMode::Tui
    ));
    assert!(!should_watch_repository(
        RepoDetectionSource::CloudEnvironmentPrep,
        SettingsMode::Tui
    ));
}

#[test]
fn global_provider_initial_scans_cover_claude_codex_and_agents() {
    let home = PathBuf::from("/home/test");

    assert_eq!(home_subdir_to_watch(MCPProvider::Claude), None);
    assert_eq!(
        home.join(MCPProvider::Claude.home_config_path()),
        home.join(".claude.json")
    );

    for (provider, subdir, config) in [
        (MCPProvider::Codex, ".codex", ".codex/config.toml"),
        (MCPProvider::Agents, ".agents", ".agents/.mcp.json"),
    ] {
        assert_eq!(home_subdir_to_watch(provider), Some(PathBuf::from(subdir)));
        let discovered =
            providers_in_scope(home.clone(), home.join(subdir)).collect::<HashSet<_>>();
        assert!(
            discovered.contains(&(provider, home.join(config))),
            "{provider:?} config should be included in its home subdirectory scan"
        );
    }
}

#[test]
fn project_initial_scan_covers_each_supported_provider_config() {
    let repo = PathBuf::from("/work/repository");
    let discovered = providers_in_scope(repo.clone(), repo.clone()).collect::<HashSet<_>>();

    for provider in [
        MCPProvider::Warp,
        MCPProvider::Claude,
        MCPProvider::Codex,
        MCPProvider::Agents,
    ] {
        assert!(
            discovered.contains(&(provider, repo.join(provider.project_config_path()))),
            "{provider:?} project config should be included in a repository scan"
        );
    }
}

#[test]
fn incremental_updates_detect_each_supported_provider_config() {
    let repo = PathBuf::from("/work/repository");
    for provider in [
        MCPProvider::Warp,
        MCPProvider::Claude,
        MCPProvider::Codex,
        MCPProvider::Agents,
    ] {
        let config_path = repo.join(provider.project_config_path());
        let mut added = RepositoryUpdate::default();
        added
            .added
            .insert(TargetFile::new(config_path.clone(), false));
        assert_eq!(config_change_flags(&added, &config_path), (false, true));

        let mut deleted = RepositoryUpdate::default();
        deleted
            .deleted
            .insert(TargetFile::new(config_path.clone(), false));
        assert_eq!(config_change_flags(&deleted, &config_path), (true, false));
    }
}
#[test]
fn test_substitute_env_vars_success() {
    let test_vars = ["FOO", "BAZ", "REPEATED"];

    // Setup environment variables
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var("FOO", "bar") };
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var("BAZ", "qux") };
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var("REPEATED", "value") };

    // Test 1: Single variable substitution
    let input = r#"{"key": "${FOO}"}"#;
    let result = substitute_env_vars(input).expect("Single variable substitution should succeed");
    assert_eq!(
        result, r#"{"key": "bar"}"#,
        "Single variable FOO should be replaced with 'bar'"
    );

    // Test 2: Multiple different variables
    let input = r#"{"key": "${FOO}", "other": "${BAZ}"}"#;
    let result = substitute_env_vars(input).expect("Multiple variable substitution should succeed");
    assert_eq!(
        result, r#"{"key": "bar", "other": "qux"}"#,
        "Multiple variables FOO and BAZ should be replaced"
    );

    // Test 3: Multiple occurrences of same variable
    let input = r#"{"a": "${REPEATED}", "b": "${REPEATED}", "c": "prefix_${REPEATED}_suffix"}"#;
    let result = substitute_env_vars(input).expect("Repeated variable substitution should succeed");
    assert_eq!(
        result, r#"{"a": "value", "b": "value", "c": "prefix_value_suffix"}"#,
        "All occurrences of REPEATED should be replaced with 'value', including within context"
    );

    // Cleanup
    cleanup_env_vars(&test_vars);
}

#[test]
fn test_substitute_env_vars_missing_or_empty() {
    // Test 1: Missing variable
    // Ensure MISSING_VAR is not set
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::remove_var("MISSING_VAR") };

    let input = r#"{"key": "${MISSING_VAR}"}"#;
    let result = substitute_env_vars(input);
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Missing or empty environment variable: MISSING_VAR"),
        "Error message should mention MISSING_VAR, got: {err_msg}"
    );

    // Test 2: Empty variable
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var("EMPTY_VAR", "") };

    let input = r#"{"key": "${EMPTY_VAR}"}"#;
    let result = substitute_env_vars(input);
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Missing or empty environment variable: EMPTY_VAR"),
        "Error message should mention EMPTY_VAR, got: {err_msg}"
    );

    // Cleanup
    cleanup_env_vars(&["EMPTY_VAR"]);
}

#[tokio::test]
async fn parse_outcomes_distinguish_missing_invalid_and_valid_configs() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join(".mcp.json");

    assert!(matches!(
        parse_mcp_config_file(&path, MCPProvider::Warp).await,
        FileMCPConfigParseOutcome::Missing
    ));

    std::fs::write(&path, "{invalid").expect("invalid config should be written");
    match parse_mcp_config_file(&path, MCPProvider::Warp).await {
        FileMCPConfigParseOutcome::Error(diagnostic) => {
            assert_eq!(diagnostic.kind, FileMCPConfigDiagnosticKind::Parse);
        }
        _ => panic!("invalid JSON should produce a parse diagnostic"),
    }

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("WARP_MCP_TEST_MISSING") };
    std::fs::write(
        &path,
        r#"{"mcpServers":{"test":{"command":"${WARP_MCP_TEST_MISSING}"}}}"#,
    )
    .expect("missing-env config should be written");
    match parse_mcp_config_file(&path, MCPProvider::Warp).await {
        FileMCPConfigParseOutcome::Error(diagnostic) => {
            assert_eq!(
                diagnostic.kind,
                FileMCPConfigDiagnosticKind::MissingEnvironmentVariable
            );
        }
        _ => panic!("missing env should produce a diagnostic"),
    }

    std::fs::write(
        &path,
        r#"{"mcpServers":{"test":{"command":"test-command"}}}"#,
    )
    .expect("valid config should be written");
    match parse_mcp_config_file(&path, MCPProvider::Warp).await {
        FileMCPConfigParseOutcome::Parsed(servers) => assert_eq!(servers.len(), 1),
        _ => panic!("valid config should produce one server"),
    }
}

/// Test-only collector model. A separate model is required to subscribe to `FileMCPWatcher`
/// events in tests: a model may not subscribe to its own events, so the assertions below
/// (`watch_initial_global_scan_completions`) subscribe from this standalone entity instead.
struct WatcherEventCollector;

impl warpui::Entity for WatcherEventCollector {
    type Event = ();
}

/// Subscribes to `FileMCPWatcher` and returns a future that resolves once
/// `InitialGlobalMcpScanComplete` has been observed `expected_count` times, using a shared
/// counter so callers can also assert an exact emission count after the fact.
fn watch_initial_global_scan_completions(
    app: &mut App,
    watcher: &warpui::ModelHandle<FileMCPWatcher>,
    expected_count: usize,
) -> futures::channel::oneshot::Receiver<()> {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    let mut tx = Some(tx);
    let count = std::rc::Rc::new(std::cell::RefCell::new(0usize));
    let collector = app.add_model(|_| WatcherEventCollector);
    collector.update(app, |_, ctx| {
        ctx.subscribe_to_model(watcher, move |_, _, event, _| {
            if matches!(event, FileMCPWatcherEvent::InitialGlobalMcpScanComplete) {
                *count.borrow_mut() += 1;
                if *count.borrow() == expected_count
                    && let Some(sender) = tx.take()
                {
                    let _ = sender.send(());
                }
            }
        });
    });
    // Leak the collector so it (and its subscription) outlives this function; tests are
    // short-lived, so this is acceptable.
    std::mem::forget(collector);
    rx
}

/// The initial global scan must settle once every scheduled source has produced a terminal
/// parse outcome, whether that outcome is a valid parse, a missing file, or an invalid config.
#[test]
fn initial_global_scan_settles_after_parsed_missing_and_invalid_sources() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let root = dir.path().to_path_buf();
    let parsed_path = root.join("parsed.json");
    std::fs::write(&parsed_path, r#"{"mcpServers":{"test":{"command":"npx"}}}"#).unwrap();
    let missing_path = root.join("missing.json");
    let invalid_path = root.join("invalid.json");
    std::fs::write(&invalid_path, "{invalid").unwrap();

    let pending = HashSet::from([
        (parsed_path.clone(), MCPProvider::Warp),
        (missing_path.clone(), MCPProvider::Claude),
        (invalid_path.clone(), MCPProvider::Codex),
    ]);

    App::test((), |mut app| async move {
        let watcher = setup_watcher_with_pending(&mut app, pending);
        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);

        watcher.update(&mut app, |watcher, ctx| {
            watcher.update_servers_from_config_file(
                &parsed_path,
                root.clone(),
                MCPProvider::Warp,
                ctx,
            );
            watcher.update_servers_from_config_file(
                &missing_path,
                root.clone(),
                MCPProvider::Claude,
                ctx,
            );
            watcher.update_servers_from_config_file(
                &invalid_path,
                root.clone(),
                MCPProvider::Codex,
                ctx,
            );
        });

        rx.await
            .expect("initial global scan should settle after mixed terminal outcomes");
        watcher.read(&app, |watcher, _| {
            assert!(
                watcher.initial_global_scan_pending.is_empty(),
                "pending set should be drained once every source settles"
            );
        });
    });
}

/// `InitialGlobalMcpScanComplete` must fire exactly once, even if settlement logic runs again
/// afterward (e.g. a later, unrelated parse completion).
#[test]
fn initial_global_scan_completion_event_fires_exactly_once() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let root = dir.path().to_path_buf();
    let missing_path = root.join("missing.json");
    let pending = HashSet::from([(missing_path.clone(), MCPProvider::Warp)]);

    App::test((), |mut app| async move {
        let watcher = setup_watcher_with_pending(&mut app, pending);
        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);

        watcher.update(&mut app, |watcher, ctx| {
            watcher.update_servers_from_config_file(
                &missing_path,
                root.clone(),
                MCPProvider::Warp,
                ctx,
            );
        });
        rx.await.expect("initial global scan should settle");

        // Driving the completion check again after settlement (as a later, unrelated parse
        // completion would) must not re-emit the event. There is no positive event to await
        // here (that's the point), so bound the wait instead of hanging forever.
        let second_rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);
        watcher.update(&mut app, |watcher, ctx| {
            watcher.maybe_emit_initial_global_scan_complete(ctx);
        });
        use warpui::r#async::FutureExt as _;
        assert!(
            second_rx
                .with_timeout(std::time::Duration::from_millis(200))
                .await
                .is_err(),
            "a second subscriber should never observe a second completion event"
        );
    });
}

/// If an initial parse is aborted because a file update schedules a replacement (e.g. the file
/// changed while the initial parse was still in flight), the replacement's completion must
/// still settle the initial-scan obligation for that source.
#[test]
fn replaced_initial_parse_settles_via_replacement() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let root = dir.path().to_path_buf();
    let config_path = root.join("config.json");
    std::fs::write(&config_path, r#"{"mcpServers":{"test":{"command":"npx"}}}"#).unwrap();
    let pending = HashSet::from([(config_path.clone(), MCPProvider::Warp)]);

    App::test((), |mut app| async move {
        let watcher = setup_watcher_with_pending(&mut app, pending);
        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);

        watcher.update(&mut app, |watcher, ctx| {
            // Simulate an in-flight initial parse for this key that is about to be aborted.
            let (abort_handle, _registration) = AbortHandle::new_pair();
            watcher.in_flight_parses.insert(
                (config_path.clone(), MCPProvider::Warp),
                InFlightParse {
                    generation: 0,
                    abort_handle,
                },
            );

            // A file update schedules a replacement parse for the same key. This aborts the
            // simulated in-flight parse above and spawns a new one; the pending set must still
            // contain the key so the replacement's completion settles the scan.
            watcher.update_servers_from_config_file(
                &config_path,
                root.clone(),
                MCPProvider::Warp,
                ctx,
            );
            assert!(
                watcher
                    .initial_global_scan_pending
                    .contains(&(config_path.clone(), MCPProvider::Warp)),
                "the obligation must transfer to the replacement, not be dropped on abort"
            );
        });

        rx.await
            .expect("the replacement parse should settle the initial scan");
    });
}

/// The core race behind a superseded parse's completion callback: its background future can
/// already be queued on the foreground executor before a replacement's `AbortHandle::abort()`
/// call takes effect (the framework applies `abort()` only the next time the aborted future is
/// polled). Unlike [`replaced_initial_parse_settles_via_replacement`] (which only parks an inert
/// `AbortHandle`, never invoking any callback logic), this drives the actual generation check a
/// stale callback runs through: it must not be able to reclaim the source once a replacement has
/// taken it over, and the replacement's own record must survive untouched.
#[test]
fn stale_completion_callback_cannot_reclaim_a_superseded_source() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let root = dir.path().to_path_buf();
    let config_path = root.join("config.json");
    std::fs::write(&config_path, r#"{"mcpServers":{"test":{"command":"npx"}}}"#).unwrap();
    let key = (config_path.clone(), MCPProvider::Warp);
    let pending = HashSet::from([key.clone()]);

    App::test((), |mut app| async move {
        let watcher = setup_watcher_with_pending(&mut app, pending);
        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);

        let stale_generation = watcher.update(&mut app, |watcher, ctx| {
            // Schedule the original parse ("A") for this source and capture the generation a
            // completion callback for it would have captured.
            watcher.update_servers_from_config_file(
                &config_path,
                root.clone(),
                MCPProvider::Warp,
                ctx,
            );
            let stale_generation = watcher.in_flight_parses[&key].generation;

            // Schedule a replacement ("B") for the same source before A's completion runs, as
            // a rapid file edit would. This is the moment A's callback could already be queued
            // on the foreground executor, ahead of the `abort()` inside this same call taking
            // effect on A's background future.
            watcher.update_servers_from_config_file(
                &config_path,
                root.clone(),
                MCPProvider::Warp,
                ctx,
            );
            let current_generation = watcher.in_flight_parses[&key].generation;
            assert_ne!(
                stale_generation, current_generation,
                "the replacement must claim a fresh generation, not reuse A's"
            );

            stale_generation
        });

        // A's now-stale completion callback fires (out of process order): it must not be able
        // to reclaim the source, since B's record currently owns it.
        let reclaimed = watcher.update(&mut app, |watcher, _ctx| {
            watcher.take_current_in_flight_parse(&key, stale_generation)
        });
        assert!(
            !reclaimed,
            "a stale callback must not be able to reclaim a source superseded by a replacement"
        );
        watcher.read(&app, |watcher, _| {
            assert!(
                watcher.in_flight_parses.contains_key(&key),
                "the stale callback must not have removed the replacement's own record"
            );
            assert!(
                watcher.initial_global_scan_pending.contains(&key),
                "the stale callback must not have claimed or settled the cohort obligation"
            );
        });

        // The replacement (B) itself, once it actually completes, must still settle the scan.
        rx.await
            .expect("the replacement's own completion should settle the initial scan");
    });
}

/// Subscribes to `FileMCPWatcher` and returns a future that resolves with the `scan_origin` of
/// the first `ConfigParsed` event observed for `provider`.
fn watch_config_parsed_scan_origin(
    app: &mut App,
    watcher: &warpui::ModelHandle<FileMCPWatcher>,
    provider: MCPProvider,
) -> futures::channel::oneshot::Receiver<FileMCPScanOrigin> {
    let (tx, rx) = futures::channel::oneshot::channel::<FileMCPScanOrigin>();
    let mut tx = Some(tx);
    let collector = app.add_model(|_| WatcherEventCollector);
    collector.update(app, |_, ctx| {
        ctx.subscribe_to_model(watcher, move |_, _, event, _| {
            if let FileMCPWatcherEvent::ConfigParsed {
                provider: event_provider,
                scan_origin,
                ..
            } = event
                && *event_provider == provider
                && let Some(sender) = tx.take()
            {
                let _ = sender.send(*scan_origin);
            }
        });
    });
    // Leak the collector so it (and its subscription) outlives this function; tests are
    // short-lived, so this is acceptable.
    std::mem::forget(collector);
    rx
}

/// If the directory watcher's registration for a home-subdir provider fails
/// *asynchronously* -- after `start_watching` already queued its initial scan -- `stop_watching`
/// removes the subscription before that scan can find it, so no `on_scan` (and so no config
/// parse) ever arrives. Regression test for the resulting stall: `settle_stranded_subdir_configs`
/// (called from that failure handler) must parse the affected provider config directly,
/// settling any pending initial-scan obligation instead of leaving it to block until the
/// caller's timeout.
#[test]
fn registration_failure_settles_stranded_subdir_provider_directly() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let home_dir = dir.path().to_path_buf();
    let codex_dir = home_dir.join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("codex subdir should be created");
    let codex_config_path = codex_dir.join("config.toml");
    std::fs::write(
        &codex_config_path,
        "[mcp_servers.test-codex-server]\ncommand = \"npx\"\nargs = [\"-y\", \"test-server\"]\n",
    )
    .expect("codex config should be written");

    let pending = HashSet::from([(codex_config_path, MCPProvider::Codex)]);

    App::test((), |mut app| async move {
        let watcher = setup_watcher_with_pending(&mut app, pending);
        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);
        let parsed_rx = watch_config_parsed_scan_origin(&mut app, &watcher, MCPProvider::Codex);

        watcher.update(&mut app, |watcher, ctx| {
            watcher.settle_stranded_subdir_configs(&codex_dir, home_dir.clone(), ctx);
        });

        rx.await
            .expect("the direct parse from the failure handler must settle the initial scan");
        let scan_origin = parsed_rx
            .await
            .expect("the direct parse for Codex should have been observed");
        assert_eq!(
            scan_origin,
            FileMCPScanOrigin::InitialGlobal,
            "the stranded source's direct parse must still be attributed to the initial \
             global scan"
        );
    });
}

/// The stranded-subdir fallback must not re-read a source that has already settled by the
/// time it runs (e.g. the watcher's queued scan delivered and completed before an async
/// registration failure was even reported): re-reading would be a second filesystem read and a
/// second `ConfigParsed` reconciliation for no benefit, violating the one-read-per-initial-
/// source invariant.
#[test]
fn settle_stranded_subdir_configs_skips_an_already_settled_source() {
    let dir = tempfile::tempdir().expect("temp dir should be created");
    let home_dir = dir.path().to_path_buf();
    let codex_dir = home_dir.join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("codex subdir should be created");
    let codex_config_path = codex_dir.join("config.toml");
    std::fs::write(
        &codex_config_path,
        "[mcp_servers.test-codex-server]\ncommand = \"npx\"\nargs = [\"-y\", \"test-server\"]\n",
    )
    .expect("codex config should be written");

    let pending = HashSet::from([(codex_config_path.clone(), MCPProvider::Codex)]);

    App::test((), |mut app| async move {
        let watcher = setup_watcher_with_pending(&mut app, pending);
        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);

        // Simulate the watcher's queued scan delivering and settling the source first -- the
        // non-stranded case: an ordinary parse for this source completes and drains the
        // cohort, exactly as the real `on_scan`-triggered path would.
        watcher.update(&mut app, |watcher, ctx| {
            watcher.update_servers_from_config_file(
                &codex_config_path,
                home_dir.clone(),
                MCPProvider::Codex,
                ctx,
            );
        });
        rx.await
            .expect("the delivered parse should settle the initial scan");

        // A second `ConfigParsed` for this source after settlement would mean the fallback
        // re-read it.
        let second_parse_rx =
            watch_config_parsed_scan_origin(&mut app, &watcher, MCPProvider::Codex);

        // The registration-failure fallback runs anyway (e.g. the async failure was reported
        // after the scan already settled the source); it must be a no-op now.
        watcher.update(&mut app, |watcher, ctx| {
            watcher.settle_stranded_subdir_configs(&codex_dir, home_dir.clone(), ctx);
        });

        use warpui::r#async::FutureExt as _;
        assert!(
            second_parse_rx
                .with_timeout(std::time::Duration::from_millis(200))
                .await
                .is_err(),
            "the fallback must not re-read an already-settled source"
        );
    });
}

/// An existing home-subdir provider (e.g. `~/.codex`) must still be awaited by the initial
/// global scan even though its read is delivered by the directory watcher's queued `on_scan`
/// rather than a direct parse scheduled in `FileMCPWatcher::new`. Regression test for a bug
/// where such sources were silently excluded from the cohort -- letting
/// `InitialGlobalMcpScanComplete` fire (and settle `AgentDriver`'s first-turn wait) before the
/// watcher-delivered read ever happened, reintroducing the first-turn-missing-tools bug for
/// exactly the users who have that provider's config file.
#[test]
#[serial_test::serial]
fn initial_global_scan_awaits_existing_subdir_provider_via_watcher() {
    let home = tempfile::tempdir().expect("temp home should be created");
    let codex_dir = home.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("codex subdir should be created");
    let codex_config_path = codex_dir.join("config.toml");
    std::fs::write(
        &codex_config_path,
        "[mcp_servers.test-codex-server]\ncommand = \"npx\"\nargs = [\"-y\", \"test-server\"]\n",
    )
    .expect("codex config should be written");

    let old_home = std::env::var_os("HOME");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("HOME", home.path()) };

    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let watcher = app.add_singleton_model(FileMCPWatcher::new);

        // The cohort must still be awaiting the Codex source right after construction: it was
        // not scheduled as a direct parse (watching started), but it must not have been
        // dropped from the cohort either.
        watcher.read(&app, |watcher, _| {
            assert!(
                watcher
                    .initial_global_scan_pending
                    .contains(&(codex_config_path.clone(), MCPProvider::Codex)),
                "an existing subdir provider must remain in the initial-global-scan cohort \
                 while its watcher-delivered read is still pending"
            );
        });

        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);
        let parsed_rx = watch_config_parsed_scan_origin(&mut app, &watcher, MCPProvider::Codex);

        rx.await
            .expect("the watcher-delivered read must still settle the initial scan");
        let scan_origin = parsed_rx
            .await
            .expect("the watcher-delivered ConfigParsed for Codex should have been observed");
        assert_eq!(
            scan_origin,
            FileMCPScanOrigin::InitialGlobal,
            "the watcher-delivered ConfigParsed for an existing subdir provider must be \
             attributed to the initial global scan, not treated as an ordinary update"
        );
    });

    match old_home {
        // TODO: Audit that the environment access only happens in single-threaded code.
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        // TODO: Audit that the environment access only happens in single-threaded code.
        None => unsafe { std::env::remove_var("HOME") },
    }
}

/// A home-subdir provider whose directory doesn't exist yet must still settle immediately via
/// a direct parse, exactly as before the cohort-membership fix above: that fix only changes
/// *which sources are awaited*, never which path delivers a missing subdir's read.
#[test]
#[serial_test::serial]
fn initial_global_scan_settles_missing_subdir_provider_via_direct_parse() {
    let home = tempfile::tempdir().expect("temp home should be created");
    let codex_config_path = home.path().join(".codex").join("config.toml");
    // Deliberately do not create `.codex`, so `watch_home_provider_dir` fails synchronously
    // and `FileMCPWatcher::new` falls back to scheduling a direct parse for the Codex source.

    let old_home = std::env::var_os("HOME");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("HOME", home.path()) };

    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let watcher = app.add_singleton_model(FileMCPWatcher::new);

        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);
        rx.await
            .expect("a missing subdir provider must still settle via a direct parse");
        watcher.read(&app, |watcher, _| {
            assert!(
                !watcher
                    .initial_global_scan_pending
                    .contains(&(codex_config_path.clone(), MCPProvider::Codex)),
                "a missing subdir provider's direct parse must settle its cohort obligation"
            );
        });
    });

    match old_home {
        // TODO: Audit that the environment access only happens in single-threaded code.
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        // TODO: Audit that the environment access only happens in single-threaded code.
        None => unsafe { std::env::remove_var("HOME") },
    }
}

/// A config removal with no replacement parse (e.g. the file was deleted) must still settle a
/// pending initial-scan source; otherwise the scan would hang forever.
#[test]
fn aborted_initial_parse_without_replacement_settles_scan() {
    let config_path = PathBuf::from("/tmp/removed-during-initial-scan.json");
    let pending = HashSet::from([(config_path.clone(), MCPProvider::Warp)]);

    App::test((), |mut app| async move {
        let watcher = setup_watcher_with_pending(&mut app, pending);
        let rx = watch_initial_global_scan_completions(&mut app, &watcher, 1);

        watcher.update(&mut app, |watcher, ctx| {
            let (abort_handle, _registration) = AbortHandle::new_pair();
            watcher.in_flight_parses.insert(
                (config_path.clone(), MCPProvider::Warp),
                InFlightParse {
                    generation: 0,
                    abort_handle,
                },
            );

            // The config was removed outright; no replacement parse follows.
            watcher.abort_config_parse_for_removal(&config_path, MCPProvider::Warp, ctx);
        });

        rx.await
            .expect("removal without a replacement must still settle the initial scan");
        watcher.read(&app, |watcher, _| {
            assert!(watcher.initial_global_scan_pending.is_empty());
        });
    });
}
