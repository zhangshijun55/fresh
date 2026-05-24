//! VERIFICATION (plugins ON) for issue #2056.
//!
//! The plugins-OFF verification proved the core bring-up roots the
//! file-explorer + title at the launch cwd even though the worktree
//! session is the active window. This test repeats the observation
//! with the embedded orchestrator plugin LOADED, to see whether the
//! plugin's startup behavior moves `working_dir` to the worktree (which
//! would re-root the explorer/title and reproduce the screenshot).
//!
//! It is an OBSERVATION test: it prints the measured values and asserts
//! the truth discovered on first run.

#![cfg(feature = "plugins")]

mod common;

use common::harness::{EditorTestHarness, HarnessOptions};
use fresh::config::Config;
use fresh::config_io::DirectoryContext;
use std::path::{Path, PathBuf};

fn json_path(p: &Path) -> String {
    serde_json::to_string(p).unwrap().trim_matches('"').to_string()
}

#[test]
fn observe_rendered_root_with_orchestrator_plugin_loaded() {
    fresh::i18n::set_locale("en");

    let sandbox = tempfile::tempdir().unwrap();
    let mk = |n: &str| {
        let p = sandbox.path().join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    };
    let data_home = mk("data-home");
    let project = mk("project");
    let worktree = mk("worktree");
    std::fs::write(project.join("PROJECT_FILE.md"), "p").unwrap();
    std::fs::write(worktree.join("WORKTREE_FILE.md"), "w").unwrap();

    let dir_context = DirectoryContext::for_testing(&data_home);
    let orch = dir_context.data_dir.join("orchestrator");
    std::fs::create_dir_all(&orch).unwrap();
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/orchestrator_bringup/v2_worktree_session.json"
    ))
    .unwrap()
    .replace("__PROJECT__", &json_path(&project))
    .replace("__WORKTREE__", &json_path(&worktree));
    std::fs::write(orch.join("windows.json"), fixture).unwrap();

    let config = Config {
        check_for_updates: false,
        ..Config::default()
    };
    // `without_empty_plugins_dir` enables embedded-plugin loading (the
    // orchestrator), the same path the orchestrator e2e tests use.
    let mut h = EditorTestHarness::create(
        120,
        40,
        HarnessOptions::new()
            .with_working_dir(project.clone())
            .with_shared_dir_context(dir_context)
            .with_config(config)
            .without_empty_plugins_dir(),
    )
    .unwrap();

    // Phase C restore + inactive-window restore.
    h.startup(true, &[]).unwrap();
    h.editor_mut().restore_inactive_window_workspaces();

    // Let plugins finish loading and run any startup hooks
    // (editor_initialized / ready). Generous bounded pump.
    for _ in 0..400 {
        h.render().unwrap();
        h.editor_mut().process_async_messages();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // Phase D: open the file explorer, pump until its tree initializes.
    h.editor_mut().show_file_explorer();
    let mut explorer_root: Option<PathBuf> = None;
    for _ in 0..100 {
        h.render().unwrap();
        h.editor_mut().process_async_messages();
        if let Some(v) = h.editor().file_explorer() {
            explorer_root = Some(v.tree().root_path().to_path_buf());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    let working_dir = h.editor().working_dir().to_path_buf();
    let active_root = h.editor().active_window().root.clone();
    let title_project = working_dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());

    eprintln!("=== OBSERVED (worktree-hijack, orchestrator plugin LOADED) ===");
    eprintln!("project            = {}", project.display());
    eprintln!("worktree           = {}", worktree.display());
    eprintln!("active_window.root = {}", active_root.display());
    eprintln!("editor.working_dir = {}", working_dir.display());
    eprintln!("file_explorer root = {:?}", explorer_root);
    eprintln!("title project name = {:?}", title_project);
    eprintln!("session_count      = {}", h.editor().session_count());

    // Known from phase B.
    assert_eq!(active_root, worktree, "active window root is the worktree");

    // VERIFIED: loading the orchestrator plugin does NOT move
    // working_dir / the explorer root to the worktree. The plugin does
    // not auto-activate the persisted session on startup, so the
    // explorer and title still root at the launch cwd — the screenshot
    // symptom is reproduced by NEITHER the core pick nor the plugin.
    let explorer_root = explorer_root.expect("file explorer should initialize");
    assert_eq!(working_dir, project, "working_dir stays at the launch cwd even with the plugin loaded");
    assert_eq!(explorer_root, project, "file-explorer still roots at the cwd with the plugin loaded");
    assert_eq!(title_project.as_deref(), Some("project"), "title still shows the cwd with the plugin loaded");
}
