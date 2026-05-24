//! VERIFICATION (observation) test for issue #2056.
//!
//! The characterization suite proved the worktree session becomes the
//! `active_window().root`. This test goes one step further — the step
//! that was previously analysis-only — and OBSERVES the rendered
//! file-explorer root, the editor `working_dir`, and the derived window
//! title after a faithful bring-up (phase B construct + phase C restore
//! + phase D file-explorer init), to confirm whether the hijack
//! actually reaches what the user sees in the screenshots.
//!
//! Plugins are OFF: this isolates the Rust core bring-up. If the
//! explorer/title root at the PROJECT here, the screenshot symptom is
//! NOT produced by the core pick alone and must come from a later step
//! (a dive / set_active_window, or the orchestrator plugin).

mod common;

use common::harness::{EditorTestHarness, HarnessOptions};
use fresh::config::Config;
use fresh::config_io::DirectoryContext;
use fresh_core::WindowId;
use std::path::{Path, PathBuf};

fn json_path(p: &Path) -> String {
    serde_json::to_string(p).unwrap().trim_matches('"').to_string()
}

/// Build a harness in `project` with the worktree-hijack fixture
/// planted, run phase-C restore. Returns (harness, project, worktree).
fn hijack_harness() -> (EditorTestHarness, PathBuf, PathBuf, tempfile::TempDir) {
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
    let mut h = EditorTestHarness::create(
        100,
        40,
        HarnessOptions::new()
            .with_working_dir(project.clone())
            .with_shared_dir_context(dir_context)
            .with_config(config)
            .with_empty_plugins_dir(),
    )
    .unwrap();
    h.startup(true, &[]).unwrap();
    h.editor_mut().restore_inactive_window_workspaces();
    (h, project, worktree, sandbox)
}

fn pump_explorer_root(h: &mut EditorTestHarness) -> Option<PathBuf> {
    h.editor_mut().show_file_explorer();
    for _ in 0..50 {
        h.render().unwrap();
        h.editor_mut().process_async_messages();
        if let Some(v) = h.editor().file_explorer() {
            return Some(v.tree().root_path().to_path_buf());
        }
    }
    None
}

#[test]
fn observe_rendered_root_under_worktree_hijack() {
    fresh::i18n::set_locale("en");

    // Sandbox: data dir (holds windows.json), project (cwd), worktree.
    let sandbox = tempfile::tempdir().unwrap();
    let mk = |n: &str| {
        let p = sandbox.path().join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    };
    let data_home = mk("data-home");
    let project = mk("project");
    let worktree = mk("worktree");
    // Put a recognizable file in each so a rendered tree is distinguishable.
    std::fs::write(project.join("PROJECT_FILE.md"), "p").unwrap();
    std::fs::write(worktree.join("WORKTREE_FILE.md"), "w").unwrap();

    // Plant the real-captured worktree-hijack fixture at the v2 global
    // location, with this run's paths substituted in.
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

    // Build the editor in the project dir, sharing the planted data dir.
    let config = Config {
        check_for_updates: false,
        ..Config::default()
    };
    let mut h = EditorTestHarness::create(
        100,
        40,
        common::harness::HarnessOptions::new()
            .with_working_dir(project.clone())
            .with_shared_dir_context(dir_context)
            .with_config(config)
            .with_empty_plugins_dir(),
    )
    .unwrap();

    // Phase C: restore (mirrors handle_first_run_setup), plus the
    // inactive-window restore main.rs runs right after.
    h.startup(true, &[]).unwrap();
    h.editor_mut().restore_inactive_window_workspaces();

    // Phase D: open the file explorer and pump async until its tree
    // initializes (init_file_explorer spawns a tokio task).
    h.editor_mut().show_file_explorer();
    let mut explorer_root: Option<PathBuf> = None;
    for _ in 0..50 {
        h.render().unwrap();
        h.editor_mut().process_async_messages();
        if let Some(v) = h.editor().file_explorer() {
            explorer_root = Some(v.tree().root_path().to_path_buf());
            break;
        }
    }

    let working_dir = h.editor().working_dir().to_path_buf();
    let active_root = h.editor().active_window().root.clone();
    let title_project = working_dir
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());

    eprintln!("=== OBSERVED (worktree-hijack, plugins off) ===");
    eprintln!("project            = {}", project.display());
    eprintln!("worktree           = {}", worktree.display());
    eprintln!("active_window.root = {}", active_root.display());
    eprintln!("editor.working_dir = {}", working_dir.display());
    eprintln!("file_explorer root = {:?}", explorer_root);
    eprintln!("title project name = {:?}", title_project);
    let screen = h.screen_to_string();
    eprintln!(
        "explorer shows PROJECT_FILE.md = {}",
        screen.contains("PROJECT_FILE")
    );
    eprintln!(
        "explorer shows WORKTREE_FILE.md = {}",
        screen.contains("WORKTREE_FILE")
    );

    // The fact already proven (phase B): the active window is rooted at
    // the worktree.
    assert_eq!(active_root, worktree, "active window root is the worktree (known)");

    // THE VERIFIED RESULT: the hijack does NOT reach the rendered UI.
    // `working_dir` stays at the launch cwd through the whole core
    // bring-up, and BOTH the file-explorer root and the window title
    // derive from `working_dir` — so they root at the PROJECT, not the
    // worktree. The screenshot symptom (explorer/title showing a foreign
    // dir) is therefore NOT produced by the core active-window pick; it
    // must come from a later step that moves `working_dir` to a session
    // root (e.g. the orchestrator plugin activating the persisted session
    // on startup, or a user dive) — which is plugins-off here.
    let explorer_root = explorer_root.expect("file explorer should initialize");
    assert_eq!(
        working_dir, project,
        "editor.working_dir stays at the launch cwd after core bring-up"
    );
    assert_eq!(
        explorer_root, project,
        "file-explorer roots at the cwd, NOT the hijacked worktree"
    );
    assert_eq!(
        title_project.as_deref(),
        Some("project"),
        "window title's project name is the cwd, NOT the worktree"
    );
}

/// Localizes the ACTUAL mechanism behind the screenshots.
///
/// At construction the active window pointer is set directly to the
/// worktree session (id 2) WITHOUT syncing `working_dir` — hence the
/// inconsistency the test above captured (active=worktree, working_dir
/// =cwd). The moment anything routes through `set_active_window` (a
/// window switch / dive), `working_dir` is set to that window's root,
/// and the file explorer re-roots there. This is the step that turns
/// the latent worktree-hijack into the visible "explorer shows a
/// foreign dir" symptom.
#[test]
fn switching_through_set_active_window_reroots_working_dir_and_explorer() {
    let (mut h, project, worktree, _sandbox) = hijack_harness();

    // At launch the active window IS the worktree (id 2), yet working_dir
    // is the cwd — construction set the active pointer directly without
    // syncing working_dir. This is the latent inconsistency.
    assert_eq!(h.editor().active_window().root, worktree);
    assert_eq!(h.editor().working_dir(), project.as_path());

    // Route through set_active_window (a window switch / dive). This is
    // the code path that syncs working_dir to the active window's root.
    h.editor_mut().set_active_window(WindowId(1));
    assert_eq!(
        h.editor().working_dir(),
        project.as_path(),
        "switching to the base window points working_dir at the project"
    );

    h.editor_mut().set_active_window(WindowId(2));
    assert_eq!(
        h.editor().working_dir(),
        worktree.as_path(),
        "switching to the worktree window points working_dir at the WORKTREE"
    );

    // The window title's project name follows working_dir live — so
    // after the switch it shows the worktree (matches the screenshots'
    // title).
    let title_after = h
        .editor()
        .working_dir()
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string);
    assert_eq!(
        title_after.as_deref(),
        Some("worktree"),
        "the title's project name becomes the worktree after the switch"
    );

    // The file explorer roots at working_dir AT FIRST-INIT TIME. Opening
    // it now (first init, while on the worktree window) roots it at the
    // worktree — the visible explorer symptom. (Note: the root is sticky;
    // an explorer already initialized at the cwd would NOT re-root on a
    // later switch — see the discovery in the test above.)
    assert_eq!(
        pump_explorer_root(&mut h),
        Some(worktree.clone()),
        "an explorer first opened while on the worktree window roots at the worktree"
    );
}
