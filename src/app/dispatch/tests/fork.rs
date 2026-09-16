use super::super::*;
use super::support::*;

#[test]
fn owner_guard_blocks_null_and_module_pane_targets() {
    let _env = crate::persist::test_env("remote-api-null-owner");
    let mut app = crate::app::remote::tests::remote_ui_app();
    let (pane, _receiver, _) = crate::app::remote::tests::add_remote_workspace(&mut app);
    for (method, params) in [
        ("pane.get", json!({"pane": null})),
        ("pane.layout", json!({"pane": null})),
        ("module.pane.focus", json!({})),
        ("module.pane.focus", json!({"pane": null})),
        ("module.pane.close", json!({"pane": pane.0.to_string()})),
    ] {
        let error = app.dispatch(method, &params).unwrap_err();
        assert_eq!(error.0, "remote_workspace", "{method}: {params}");
    }
    assert!(app.panes.is_empty());
    assert_eq!(app.workspaces.len(), 2);
}

#[test]
fn owner_guard_rejects_ignored_workspace_selectors() {
    let _env = crate::persist::test_env("remote-api-ignored-owner");
    let mut app = crate::app::remote::tests::remote_ui_app();
    let local_id = app.workspaces[0].id.clone();
    let (_pane, _receiver, _) = crate::app::remote::tests::add_remote_workspace(&mut app);
    for method in [
        "tab.rename",
        "tab.list",
        "tab.new",
        "tab.focus",
        "tab.move",
        "tab.swap",
        "tab.close",
        "workspace.new",
        "node.new",
        "files.open",
        "files.tree",
        "files.reveal",
        "files.refresh",
        "diff.list",
        "diff.open",
        "diff.refresh",
        "diff.note.remove",
        "worktree.open",
        "worktree.remove",
        "module.pane.open",
    ] {
        for selector in [
            json!({"workspace": 0}),
            json!({"workspace_id": local_id}),
            json!({"node": 0}),
        ] {
            let mut params = selector;
            params["name"] = json!("must-not-rename-the-projection");
            let error = app.dispatch(method, &params).unwrap_err();
            assert_eq!(error.0, "invalid_request", "{method}: {params}");
        }
    }
    assert!(app.panes.is_empty());
    assert_eq!(app.workspaces[1].tabs.len(), 1);
    assert_eq!(app.workspaces[1].tabs[0].name, None);
}

#[test]
fn owner_guard_keeps_explicit_local_workspace_targets_and_rejects_invalid_indices() {
    let _env = crate::persist::test_env("remote-api-explicit-owner");
    let mut app = crate::app::remote::tests::remote_ui_app();
    let local_id = app.workspaces[0].id.clone();
    let root = app.workspaces[0].cwd.clone();
    std::fs::create_dir_all(&root).unwrap();
    run_git(&root, &["init", "-q"]);
    let (_pane, _receiver, _) = crate::app::remote::tests::add_remote_workspace(&mut app);
    app.workspaces[1].cwd = crate::persist::config_dir().join("absent-remote-checkout");
    for selector in [
        json!({"workspace":0}),
        json!({"node":0}),
        json!({"workspace_id":local_id}),
    ] {
        let result = app.dispatch("worktree.list", &selector).unwrap();
        assert_eq!(
            result["worktrees"][0]["path"],
            root.canonicalize().unwrap().display().to_string()
        );
    }
    assert!(app
        .dispatch("tab.get", &json!({"workspace_id":local_id}))
        .is_ok());
    for method in [
        "git.status",
        "git.branches",
        "git.log",
        "git.open",
        "worktree.list",
        "worktree.create",
        "tab.get",
        "mission.open",
    ] {
        for selector in [
            json!({"workspace":1}),
            json!({"workspace_id":app.workspaces[1].id}),
        ] {
            assert_eq!(
                app.dispatch(method, &selector).unwrap_err().0,
                "remote_workspace",
                "{method}: {selector}"
            );
        }
        for selector in [json!({"workspace":999}), json!({"workspace_id":"missing"})] {
            assert_eq!(
                app.dispatch(method, &selector).unwrap_err().0,
                "not_found",
                "{method}: {selector}"
            );
        }
    }
    assert_eq!(app.active_ws, 1);
    assert!(app.panes.is_empty());
}

#[test]
fn opening_local_workspace_does_not_select_same_path_remote_projection() {
    let _env = crate::persist::test_env("remote-api-local-path");
    let mut app = crate::app::remote::tests::remote_ui_app();
    let path = app.workspaces[0].cwd.clone();
    let local_id = app.workspaces[0].id.clone();
    let (_pane, _receiver, _) = crate::app::remote::tests::add_remote_workspace(&mut app);
    app.workspaces[1].cwd = path.clone();
    app.workspaces.swap(0, 1);
    for method in ["workspace.open", "node.open"] {
        app.active_ws = 0;
        let result = app.dispatch(method, &json!({"path":path})).unwrap();
        assert_eq!(result["workspace"], "1", "{method}");
        assert_eq!(app.workspaces[app.active_ws].id, local_id);
    }
    assert!(app.panes.is_empty());
    assert_eq!(app.workspaces.len(), 2);
}

#[test]
fn removing_local_worktree_does_not_close_same_path_remote_projection() {
    let _env = crate::persist::test_env("remote-api-worktree-path");
    let mut app = crate::app::remote::tests::remote_ui_app();
    let repo = app.workspaces[0].cwd.clone();
    std::fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-q"]);
    run_git(
        &repo,
        &[
            "-c",
            "user.name=Luvus Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    );
    let worktree = crate::persist::config_dir().join("local-worktree-only");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-qb",
            "fixture-worktree",
            worktree.to_str().unwrap(),
        ],
    );
    app.workspaces[0].cwd = worktree.clone();
    let (_pane, _receiver, _) = crate::app::remote::tests::add_remote_workspace(&mut app);
    app.workspaces[1].cwd = worktree.clone();
    app.workspaces.swap(0, 1);
    app.active_ws = 1;
    app.dispatch("worktree.remove", &json!({"path":worktree}))
        .unwrap();
    assert_eq!(app.workspaces.len(), 1);
    assert!(
        app.workspaces[0].remote.is_some(),
        "local removal must preserve the remote owner projection"
    );
    assert!(!worktree.exists());
    assert!(app.panes.is_empty());
}
