// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

mod common;

use std::process::Command;

#[test]
fn config_show_and_validate_and_set_work() {
    let temp_dir = std::env::temp_dir().join("aman-cli-config-test");
    let _ = std::fs::create_dir_all(&temp_dir);
    let config_path = temp_dir.join("agent.yaml");
    let override_path = temp_dir.join("override.yaml");

    std::fs::write(
        &config_path,
        r#"
runtime:
  drain_timeout_sec: 10
  tool_timeout_sec: 20
security:
  risky_capabilities_enabled: false
"#,
    )
    .expect("write config");

    let bin = common::aman_cli_bin();

    let show = Command::new(&bin)
        .args(["config", "show", "--config"])
        .arg(&config_path)
        .output()
        .expect("run show");
    assert!(show.status.success());
    assert!(String::from_utf8_lossy(&show.stdout).contains("\"runtime\""));

    let validate = Command::new(&bin)
        .args(["config", "validate", "--config"])
        .arg(&config_path)
        .status()
        .expect("run validate");
    assert!(validate.success());

    let set = Command::new(&bin)
        .args([
            "config",
            "set",
            "--override",
            override_path.to_str().expect("override"),
            "--json",
            r#"{"security":{"risky_capabilities_enabled":true}}"#,
            "--config",
        ])
        .arg(&config_path)
        .status()
        .expect("run set");
    assert!(set.success());

    let show2 = Command::new(&bin)
        .args(["config", "show", "--config"])
        .arg(&config_path)
        .args(["--override"])
        .arg(&override_path)
        .output()
        .expect("run show with override");
    assert!(show2.status.success());
    assert!(String::from_utf8_lossy(&show2.stdout).contains("\"risky_capabilities_enabled\": true"));
}

