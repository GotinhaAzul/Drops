//! Exercise the actual bounded probe process and filesystem resolution without
//! requiring a compositor or reading any real application's data.
use buddy_anchor::{procfs::ProcessInfo, workspace::detect_window};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn live_window_locations_follow_navigation_and_fail_closed() {
    let base = std::env::temp_dir().join(format!("drops-live-location-{}", std::process::id()));
    let repo = base.join("project");
    let first = repo.join("first");
    let second = repo.join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let fixture = base.join("probe.json");
    let helper = base.join("probe.py");
    std::fs::write(&helper, "#!/usr/bin/python3\nimport json,os,sys\nrequest=json.loads(sys.stdin.readline())\nassert request['workspace_probe'] is True\nassert request['window']['pid']==42\nprint(open(os.environ['DROPS_TEST_PROBE_FIXTURE']).read())\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("DROPS_CAPTURE_PYTHON", &helper);
    std::env::set_var("DROPS_TEST_PROBE_FIXTURE", &fixture);
    let process = ProcessInfo {
        pid: 42,
        cwd: Some(first.to_string_lossy().into()),
        cmdline: vec!["nautilus".into(), first.to_string_lossy().into()],
        available: true,
        error: None,
    };
    let context =
        json!({"visible":true,"window":{"class":"org.gnome.Nautilus","pid":42,"title":"Files"}});
    for current in [&first, &second] {
        std::fs::write(
            &fixture,
            json!({"paths":[current],"source":"window-location","warnings":[]}).to_string(),
        )
        .unwrap();
        let result = detect_window(&context, &process, true).await;
        assert_eq!(result["status"], "detected");
        // An explorer uses its displayed folder, not the containing repository.
        assert_eq!(result["root"], current.to_string_lossy().as_ref());
    }
    std::fs::write(
        &fixture,
        json!({"paths":[first,second],"source":"window-location","warnings":[]}).to_string(),
    )
    .unwrap();
    assert_eq!(
        detect_window(&context, &process, true).await["status"],
        "ambiguous"
    );
    std::fs::write(
        &fixture,
        json!({"paths":[base.join("missing")],"source":"window-location","warnings":[]})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        detect_window(&context, &process, false).await["status"],
        "unavailable"
    );
    std::fs::write(
        &fixture,
        json!({"paths":[],"unsupported_location":true,"warnings":["Remote location"]}).to_string(),
    )
    .unwrap();
    assert_eq!(
        detect_window(&context, &process, false).await["status"],
        "unavailable"
    );
    std::fs::write(&fixture, json!({"paths":[],"warnings":[]}).to_string()).unwrap();
    // Startup args must not select the previously open folder after navigation.
    assert_eq!(
        detect_window(&context, &process, false).await["status"],
        "unavailable"
    );
    let titled = json!({"window":{"class":"org.kde.dolphin","pid":42,"title":format!("{} — Dolphin",second.display())}});
    assert_eq!(
        detect_window(&titled, &process, true).await["root"],
        second.to_string_lossy().as_ref()
    );
    let browser = json!({"window":{"class":"chromium","pid":42,"title":second.to_string_lossy()}});
    assert_eq!(
        detect_window(&browser, &process, false).await["status"],
        "unavailable"
    );
    std::env::remove_var("DROPS_CAPTURE_PYTHON");
    std::env::remove_var("DROPS_TEST_PROBE_FIXTURE");
    std::fs::remove_dir_all(base).unwrap();
}
