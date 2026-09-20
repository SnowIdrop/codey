use super::*;

fn write_subagent_test_catalog(home: &std::path::Path, models: &[&str]) {
    let path = home.join(model_catalog::relative_path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let entries = models
        .iter()
        .map(|slug| {
            serde_json::json!({
                "slug": slug, "description": "Test model", "base_instructions": "Test instructions"
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({"models": entries})).unwrap(),
    )
    .unwrap();
}

fn official_subagent_config(account_count: usize) -> CodeyConfig {
    let profiles = (0..account_count)
        .map(|index| {
            let mut profile = ProviderProfile::new(format!("Account {index}"));
            profile.source_provider_id = Some("openai".into());
            profile.auth_mode = crate::config::AUTH_MODE_OFFICIAL_ACCOUNT.into();
            profile.official_account_id = Some(format!("account-{index}"));
            profile.normalize();
            profile
        })
        .collect();
    let mut config = CodeyConfig {
        initial_route_import_completed: true,
        local_router_enabled: true,
        subagent_optimization: true,
        ..CodeyConfig::default()
    };
    config.apply_launch_official_profiles(profiles);
    let model = local_router::model_alias(config.profiles[0].provider_id(), "gpt-6-astra");
    config.subagent_model = model.clone();
    config.subagent_roles = crate::config::uniform_subagent_roles(&model, "high");
    config.normalize()
}

#[test]
fn official_subagent_models_match_catalog_and_preserve_account_selection() {
    for account_count in [1, 2] {
        let mut config = official_subagent_config(account_count);
        let last_provider = config.profiles.last().unwrap().provider_id();
        let worker_alias = local_router::model_alias(last_provider, "gpt-6-astra");
        config.subagent_roles.get_mut("codey_worker").unwrap().model = worker_alias.clone();
        let saved = config.clone();
        let runtime = router_subagent_runtime_config(&config, true).unwrap();
        let (_, catalog) = config.runtime_catalog_models();
        for selection in runtime.subagent_roles.values() {
            assert!(
                catalog.contains(&selection.model),
                "{}: {catalog:?}",
                selection.model
            );
        }
        let expected_default = if account_count == 1 {
            "gpt-6-astra"
        } else {
            &config.subagent_model
        };
        let expected_worker = if account_count == 1 {
            "gpt-6-astra"
        } else {
            &worker_alias
        };
        assert_eq!(runtime.subagent_model, expected_default);
        assert_eq!(
            runtime.subagent_roles["codey_worker"].model,
            expected_worker
        );
        assert_eq!(config, saved);
    }
}

#[test]
fn official_subagent_models_reject_ambiguous_builtin_catalog_fallback() {
    let single = official_subagent_config(1);
    let runtime = router_subagent_runtime_config(&single, false).unwrap();
    assert_eq!(runtime.subagent_model, "gpt-6-astra");
    let multiple = official_subagent_config(2);
    let error = router_subagent_runtime_config(&multiple, false).unwrap_err();
    assert!(error.to_string().contains("同名线路"), "{error}");
}

fn subagent_catalog_fallback_config() -> CodeyConfig {
    let mut route = ProviderProfile::new("Relay");
    route.id = "route-a".into();
    route.base_url = "https://relay.example/v1".into();
    route.normalize();
    CodeyConfig {
        active_profile_id: route.id.clone(),
        profiles: vec![route],
        local_router_enabled: true,
        subagent_optimization: true,
        selected_models_by_provider: std::collections::BTreeMap::from([(
            "route-a".into(),
            vec![
                "gpt-6-astra".into(),
                "gpt-5.6-terra".into(),
                "vendor/model".into(),
            ],
        )]),
        subagent_model: "route-a/gpt-6-astra".into(),
        subagent_roles: crate::config::uniform_subagent_roles("route-a/gpt-6-astra", "high"),
        ..CodeyConfig::default()
    }
}

#[test]
fn dispatch_catalog_rejects_default_alias_and_unregistered_role() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("models_cache.json"),
        r#"{"models":[{"slug":"gpt-6-astra"}]}"#,
    )
    .unwrap();
    let config = subagent_catalog_fallback_config();
    let saved = config.clone();
    let error = validate_subagent_dispatch_catalog(home.path(), &config, false).unwrap_err();
    assert!(error.to_string().contains("default_subagent_model"));
    let mut runtime = router_subagent_runtime_config(&config, false).unwrap();
    assert!(validate_subagent_dispatch_catalog(home.path(), &runtime, false).is_ok());
    runtime
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .model = "vendor/model".into();
    let error = validate_subagent_dispatch_catalog(home.path(), &runtime, false).unwrap_err();
    assert!(error.to_string().contains("codey_worker"));
    runtime
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .enabled = false;
    assert!(validate_subagent_dispatch_catalog(home.path(), &runtime, false).is_ok());
    assert_eq!(config, saved);
}

#[test]
fn dispatch_catalog_missing_or_invalid_fails_closed_unless_disabled() {
    let home = tempfile::tempdir().unwrap();
    let mut runtime = subagent_catalog_fallback_config();
    assert!(validate_subagent_dispatch_catalog(home.path(), &runtime, false).is_err());
    assert!(validate_subagent_dispatch_catalog(home.path(), &runtime, true).is_err());
    std::fs::write(home.path().join("models_cache.json"), "not-json").unwrap();
    assert!(validate_subagent_dispatch_catalog(home.path(), &runtime, false).is_err());
    runtime.subagent_optimization = false;
    assert!(validate_subagent_dispatch_catalog(home.path(), &runtime, false).is_ok());
}

#[test]
fn subagent_catalog_fallback_uses_native_ids_without_mutating_saved_routes() {
    let mut config = subagent_catalog_fallback_config();
    config.subagent_roles.get_mut("codey_worker").unwrap().model = "route-a/gpt-5.6-terra".into();
    config
        .subagent_roles
        .get_mut("codey_quick_scan")
        .unwrap()
        .reasoning_effort = "low".into();
    let saved = config.clone();
    let runtime = router_subagent_runtime_config(&config, false).unwrap();
    assert_eq!(runtime.subagent_model, "gpt-6-astra");
    assert_eq!(
        runtime.subagent_roles["codey_quick_scan"].model,
        "gpt-6-astra"
    );
    assert_eq!(
        runtime.subagent_roles["codey_quick_scan"].reasoning_effort,
        "low"
    );
    assert_eq!(
        runtime.subagent_roles["codey_worker"].model,
        "gpt-5.6-terra"
    );
    assert_eq!(config, saved);
    // A later settings save still uses the catalog mode of the running process.
    let reloaded = router_subagent_runtime_config(&config.normalize(), false).unwrap();
    assert_eq!(reloaded.subagent_roles, runtime.subagent_roles);
}

#[test]
fn subagent_catalog_fallback_preserves_configured_third_party_models() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("models_cache.json"),
        r#"{"models":[{"slug":"gpt-6-astra"},{"slug":"vendor/model"}]}"#,
    )
    .unwrap();
    let mut config = subagent_catalog_fallback_config();
    config.subagent_roles.get_mut("codey_worker").unwrap().model = "route-a/vendor/model".into();
    let saved = config.clone();
    let runtime = validated_router_subagent_runtime_config(&config, false, home.path()).unwrap();
    assert_eq!(runtime.subagent_roles["codey_worker"].model, "vendor/model");
    assert!(runtime.subagent_optimization);
    assert_eq!(config, saved);

    let runtime = startup_router_subagent_runtime_config(&mut config, false, home.path());
    assert!(runtime.subagent_optimization);
    assert_eq!(runtime.subagent_roles["codey_worker"].model, "vendor/model");
    assert_eq!(config, saved);

    write_subagent_test_catalog(
        home.path(),
        &["route-a/gpt-6-astra", "route-a/vendor/model"],
    );
    let runtime = validated_router_subagent_runtime_config(&config, true, home.path()).unwrap();
    assert_eq!(
        runtime.subagent_roles["codey_worker"].model,
        "route-a/vendor/model"
    );
    config
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .enabled = false;
    assert!(validated_router_subagent_runtime_config(&config, false, home.path()).is_ok());
}

#[test]
fn subagent_model_validation_matches_the_process_catalog_without_rewriting_user_config() {
    let home = tempfile::tempdir().unwrap();
    let mut config = subagent_catalog_fallback_config();
    config.subagent_roles.get_mut("codey_worker").unwrap().model = "route-a/vendor/model".into();
    write_subagent_test_catalog(
        home.path(),
        &["route-a/gpt-6-astra", "route-a/vendor/model"],
    );
    let generated = home.path().join(model_catalog::relative_path());
    let custom = home.path().join("custom-models.json");
    std::fs::rename(&generated, &custom).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "model_catalog_json = 'custom-models.json'\n",
    )
    .unwrap();
    // Native mode honors the user's catalog; routed mode requires its own.
    let runtime = validated_router_subagent_runtime_config(&config, false, home.path()).unwrap();
    assert_eq!(
        runtime.subagent_roles["codey_worker"].model,
        "route-a/vendor/model"
    );
    assert!(validated_router_subagent_runtime_config(&config, true, home.path()).is_err());
    std::fs::write(&custom, "{\"models\":[]}").unwrap();
    write_subagent_test_catalog(
        home.path(),
        &["route-a/gpt-6-astra", "route-a/vendor/model"],
    );
    assert!(validated_router_subagent_runtime_config(&config, false, home.path()).is_err());
    assert!(validated_router_subagent_runtime_config(&config, true, home.path()).is_ok());
    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
        "model_catalog_json = 'custom-models.json'\n"
    );
}

#[test]
fn subagent_models_must_exist_in_the_installed_catalog() {
    let home = tempfile::tempdir().unwrap();
    let mut config = subagent_catalog_fallback_config();
    config.subagent_roles.get_mut("codey_worker").unwrap().model = "route-a/vendor/model".into();
    // A valid but stale catalog from another machine or route cannot authorize
    // every configured model merely because its JSON can be loaded.
    write_subagent_test_catalog(
        home.path(),
        &["route-a/gpt-6-astra", "old-route/vendor/model"],
    );
    let error = validated_router_subagent_runtime_config(&config, true, home.path()).unwrap_err();
    assert!(error.to_string().contains("codey_worker"), "{error}");
    assert!(
        error.to_string().contains("route-a/vendor/model"),
        "{error}"
    );
    assert!(error.to_string().contains("未包含"), "{error}");

    let mut runtime = config.clone();
    let roles = startup_router_subagent_runtime_config(&mut runtime, true, home.path());
    assert!(!roles.subagent_optimization);
    runtime.subagent_optimization = true;
    assert_eq!(runtime, config);
    config
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .enabled = false;
    assert!(validated_router_subagent_runtime_config(&config, true, home.path()).is_ok());
}

#[test]
fn subagent_catalog_fallback_rejects_ambiguous_routes_but_custom_catalog_preserves_them() {
    let mut config = subagent_catalog_fallback_config();
    let mut other = config.profiles[0].clone();
    other.id = "route-b".into();
    config.profiles.push(other);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec!["gpt-6-astra".into()]);
    config.subagent_roles.get_mut("codey_worker").unwrap().model = "route-b/gpt-6-astra".into();
    let error = router_subagent_runtime_config(&config, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("同名线路"), "{error}");
    let runtime = router_subagent_runtime_config(&config, true).unwrap();
    assert_eq!(
        runtime.subagent_roles["codey_worker"].model,
        "route-b/gpt-6-astra"
    );
    assert_eq!(
        runtime.subagent_roles["codey_quick_scan"].model,
        "route-a/gpt-6-astra"
    );
}

#[test]
fn subagent_catalog_fallback_checks_enabled_roles_only_and_rejects_missing_routes() {
    let mut config = subagent_catalog_fallback_config();
    let worker = config.subagent_roles.get_mut("codey_worker").unwrap();
    worker.model = "missing/model".into();
    assert!(router_subagent_runtime_config(&config, false).is_err());
    config
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .enabled = false;
    assert!(router_subagent_runtime_config(&config, false).is_ok());
    config.subagent_optimization = false;
    assert_eq!(
        router_subagent_runtime_config(&config, false).unwrap(),
        config
    );
}

#[test]
fn subagent_catalog_fallback_disables_only_this_launch_on_invalid_routes() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("models_cache.json"),
        r#"{"models":[{"slug":"gpt-6-astra"}]}"#,
    )
    .unwrap();
    let mut config = subagent_catalog_fallback_config();
    let saved = config.clone();
    let roles = startup_router_subagent_runtime_config(&mut config, false, home.path());
    assert!(roles.subagent_optimization);
    assert_eq!(roles.subagent_model, "gpt-6-astra");
    assert_eq!(config, saved);

    let mut other = config.profiles[0].clone();
    other.id = "route-b".into();
    config.profiles.push(other);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec!["gpt-6-astra".into()]);
    let mut missing = saved;
    missing
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .model = "missing/model".into();
    let mut restored = config.clone();
    write_subagent_test_catalog(home.path(), &["route-a/gpt-6-astra", "route-b/gpt-6-astra"]);
    let roles = startup_router_subagent_runtime_config(&mut restored, true, home.path());
    assert!(roles.subagent_optimization);
    assert_eq!(roles.subagent_model, "route-a/gpt-6-astra");
    assert_eq!(restored, config);
    for saved in [config, missing] {
        let mut runtime = saved.clone();
        let roles = startup_router_subagent_runtime_config(&mut runtime, false, home.path());
        assert!(!runtime.subagent_optimization);
        assert_eq!(roles, runtime);
        runtime.subagent_optimization = true;
        assert_eq!(runtime, saved);
        // A failed startup check must not weaken validation during hot reload.
        assert!(router_subagent_runtime_config(&saved, false).is_err());
    }
}

#[tokio::test]
async fn subagent_catalog_fallback_keeps_live_routes_and_roles_until_restart() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("models_cache.json"),
        r#"{"models":[{"slug":"gpt-6-astra"},{"slug":"gpt-5.6-terra"},{"slug":"vendor/model"}]}"#,
    )
    .unwrap();
    let mut config = subagent_catalog_fallback_config();
    let mut other = config.profiles[0].clone();
    other.id = "route-b".into();
    config.profiles.push(other);
    config
        .selected_models_by_provider
        .insert("route-b".into(), vec!["other-model".into()]);
    let router = LocalRouter::start(&config).await.unwrap();
    let snapshot = Arc::clone(&router.snapshot);
    let mut runtime = CodeyRuntime {
        codex_app_path: PathBuf::new(),
        maintenance: MaintenanceStatus {
            session_status: String::new(),
            session_files_fixed: 0,
            sqlite_rows_updated: 0,
            ghost_tasks_pruned: 0,
            performance_status: String::new(),
            performance_detail: String::new(),
            startup_injection_mode: String::new(),
        },
        applied_model_config: RwLock::new(config.clone()),
        applied_subagent_config: RwLock::new(RuntimeSubagentConfig::from_config(&config)),
        applied_config: config.clone(),
        subagent_route_catalog_installed: false,
        injection_statuses: Arc::new(RwLock::new(Arc::from([]))),
        injection_scripts: cdp::prepare_injection_scripts(false, false, false, &[]),
        injection_websocket_url: Arc::new(RwLock::new(Arc::from(""))),
        child: Arc::new(Mutex::new(None)),
        process_id: None,
        #[cfg(unix)]
        process_group_id: None,
        #[cfg(target_os = "macos")]
        inspector_argument: None,
        watchdog_shutdown: Mutex::new(None),
        watchdog_task: Mutex::new(None),
        exit_watchdog_shutdown: Mutex::new(None),
        exit_watchdog_task: Mutex::new(None),
        crashpad_guard_enabled: Arc::new(AtomicBool::new(false)),
        crashpad_guard_shutdown: Mutex::new(None),
        crashpad_guard_task: Mutex::new(None),
        local_router: Some(router),
    };
    let original = runtime
        .subagent_reconcile_config(&config, home.path())
        .unwrap();
    let model = &original.subagent_roles["codey_worker"].model;
    let target = || {
        snapshot
            .read()
            .unwrap()
            .target_for_request(model, Some("route-b"), None)
            .unwrap()
            .provider_id
    };
    assert_eq!(model, "gpt-6-astra");
    assert_eq!(target(), "route-a");

    // Role edits and reordered model lists still work with the original map.
    let mut roles = config.clone();
    roles.profiles.reverse();
    roles
        .selected_models_by_provider
        .get_mut("route-a")
        .unwrap()
        .reverse();
    roles.subagent_roles.get_mut("codey_worker").unwrap().model = "route-a/gpt-5.6-terra".into();
    roles
        .subagent_roles
        .get_mut("codey_worker")
        .unwrap()
        .reasoning_effort = "low".into();
    runtime.sync_local_router_routes(&roles).unwrap();
    runtime.mark_model_config_applied(&roles).await;
    assert_eq!(runtime.applied_model_catalog_config().await, roles);
    assert!(runtime.applied_model_config().await.matches(&roles));
    let reloaded = runtime
        .subagent_reconcile_config(&roles, home.path())
        .unwrap();
    assert_eq!(
        reloaded.subagent_roles["codey_worker"].model,
        "gpt-5.6-terra"
    );
    assert_eq!(
        reloaded.subagent_roles["codey_worker"].reasoning_effort,
        "low"
    );

    let mut changed = config.clone();
    changed
        .selected_models_by_provider
        .get_mut("route-b")
        .unwrap()
        .push(model.clone());
    assert!(
        runtime
            .sync_local_router_routes(&changed)
            .unwrap_err()
            .to_string()
            .contains("需重启")
    );
    assert!(
        runtime
            .subagent_reconcile_config(&changed, home.path())
            .is_err()
    );
    assert_eq!(runtime.applied_model_catalog_config().await, roles);
    assert_eq!(target(), "route-a");

    // Removing A, or disabling optimization in saved settings, cannot release
    // the routing protection for children already running with raw model IDs.
    changed.profiles[0].enabled = false;
    changed.subagent_optimization = false;
    assert!(runtime.sync_local_router_routes(&changed).is_err());
    assert!(
        runtime
            .subagent_reconcile_config(&changed, home.path())
            .is_err()
    );
    assert_eq!(target(), "route-a");

    // Route-qualified role IDs remain safe when a custom catalog was installed.
    runtime.subagent_route_catalog_installed = true;
    runtime.sync_local_router_routes(&changed).unwrap();
    assert_eq!(target(), "route-b");
    runtime.subagent_route_catalog_installed = false;
    runtime.applied_config.subagent_optimization = false;
    runtime.sync_local_router_routes(&config).unwrap();
    assert_eq!(target(), "route-a");
    runtime.local_router.as_ref().unwrap().stop().await.unwrap();
}

fn declared_efforts(values: &[&str]) -> Vec<crate::config::ModelReasoningEffort> {
    values
        .iter()
        .map(|value| crate::config::ModelReasoningEffort {
            level: (*value).to_string(),
            value: (*value).to_string(),
        })
        .collect()
}

fn relay_profile(id: &str) -> ProviderProfile {
    let mut profile = ProviderProfile::new(id);
    profile.id = id.to_string();
    profile.base_url = format!("https://{id}.example/v1");
    profile
}

fn write_launch_official_cache(home: &std::path::Path) {
    // 生成目录需要一个带指令的模板；选择状态匹配的仍是线路声明的原始模型名。
    std::fs::write(
        home.join("models_cache.json"),
        serde_json::to_vec(&serde_json::json!({
            "models": [{
                "slug": "gpt-6-astra",
                "display_name": "GPT-6 Astra",
                "description": "Test model",
                "visibility": "list",
                "priority": 1,
                "base_instructions": "Test instructions"
            }]
        }))
        .unwrap(),
    )
    .unwrap();
}

/// 两条线路提供同名原始模型 "vendor/model"，但声明的思考等级不同。
fn declared_reasoning_scope_config() -> CodeyConfig {
    let route_a = relay_profile("route-a");
    let route_b = relay_profile("route-b");
    CodeyConfig {
        active_profile_id: route_a.id.clone(),
        profiles: vec![route_a, route_b],
        local_router_enabled: true,
        subagent_optimization: true,
        selected_models_by_provider: std::collections::BTreeMap::from([
            (
                "route-a".into(),
                vec!["vendor/model".to_string(), "limited/model".to_string()],
            ),
            ("route-b".into(), vec!["vendor/model".to_string()]),
        ]),
        upstream_models_by_provider: std::collections::BTreeMap::from([
            (
                "route-a".into(),
                vec!["vendor/model".to_string(), "limited/model".to_string()],
            ),
            ("route-b".into(), vec!["vendor/model".to_string()]),
        ]),
        model_reasoning_efforts_by_provider: std::collections::BTreeMap::from([
            (
                "route-a".into(),
                std::collections::BTreeMap::from([
                    (
                        "vendor/model".into(),
                        declared_efforts(&["low", "medium", "high", "xhigh", "max"]),
                    ),
                    ("limited/model".into(), declared_efforts(&["low", "medium"])),
                ]),
            ),
            (
                "route-b".into(),
                std::collections::BTreeMap::from([(
                    "vendor/model".into(),
                    declared_efforts(&["low"]),
                )]),
            ),
        ]),
        subagent_model: "route-a/vendor/model".into(),
        subagent_reasoning_effort: "max".into(),
        subagent_roles: crate::config::uniform_subagent_roles("route-a/vendor/model", "max"),
        ..CodeyConfig::default()
    }
}

#[tokio::test]
async fn startup_route_declarations_keep_max_and_keep_unsupported_fallbacks() {
    let home = tempfile::tempdir().unwrap();
    write_launch_official_cache(home.path());
    let mut config = declared_reasoning_scope_config();
    config
        .subagent_roles
        .get_mut(crate::config::SUBAGENT_ROLE_QUICK_SCAN)
        .unwrap()
        .model = "route-a/limited/model".into();
    let saved = config.clone();

    let profile = config.active_profile().unwrap();
    let codex_app_path = std::path::Path::new("");
    let startup = prepare_startup_model_catalog(&config, &profile, home.path(), codex_app_path)
        .await
        .unwrap();
    let mut runtime = config.clone();
    runtime.active_profile_id = profile.id;
    subagent_policy::reconcile_with_model_state(&mut runtime, Some(&startup.model_state));

    assert_eq!(
        startup
            .model_state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == "vendor/model")
            .unwrap()
            .supported_reasoning_efforts,
        ["low", "medium", "high", "xhigh", "max"]
    );
    // 生成目录时使用的带线路前缀声明仍必须保留 max。
    let generated: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.path().join(model_catalog::relative_path())).unwrap(),
    )
    .unwrap();
    let generated_declaration = generated["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["slug"] == "route-a/vendor/model")
        .unwrap();
    assert!(
        generated_declaration["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|level| level["effort"] == "max")
    );
    // 声明的 max 必须活到运行角色，而不是退回目录模板的默认深度。
    assert_eq!(
        runtime.subagent_roles[crate::config::SUBAGENT_ROLE_WORKER].reasoning_effort,
        "max"
    );
    // 真的不支持 max 的模型仍走既有回退。
    assert_eq!(
        runtime.subagent_roles[crate::config::SUBAGENT_ROLE_QUICK_SCAN].reasoning_effort,
        crate::config::DEFAULT_SUBAGENT_REASONING_EFFORT
    );
    assert_eq!(config, saved);
}

#[tokio::test]
async fn startup_route_declarations_are_scoped_to_the_current_provider() {
    let home = tempfile::tempdir().unwrap();
    write_launch_official_cache(home.path());
    let config = declared_reasoning_scope_config();
    let saved = config.clone();

    let profile = config.active_profile().unwrap();
    let codex_app_path = std::path::Path::new("");
    let startup = prepare_startup_model_catalog(&config, &profile, home.path(), codex_app_path)
        .await
        .unwrap();
    let mut route_a_runtime = config.clone();
    route_a_runtime.active_profile_id = profile.id;
    subagent_policy::reconcile_with_model_state(&mut route_a_runtime, Some(&startup.model_state));
    assert_eq!(
        route_a_runtime.subagent_roles[crate::config::SUBAGENT_ROLE_WORKER].reasoning_effort,
        "max"
    );
    assert!(
        startup
            .model_state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == "vendor/model")
            .unwrap()
            .supported_reasoning_efforts
            .iter()
            .any(|effort| effort == "max")
    );

    // 切到提供同名模型的另一条线路后，只能应用它自己的声明。
    let mut route_b = config.clone();
    route_b.active_profile_id = "route-b".into();
    route_b.subagent_model = "route-b/vendor/model".into();
    route_b.subagent_roles = crate::config::uniform_subagent_roles("route-b/vendor/model", "max");
    let profile = route_b.active_profile().unwrap();
    let startup = prepare_startup_model_catalog(&route_b, &profile, home.path(), codex_app_path)
        .await
        .unwrap();
    let mut route_b_runtime = route_b.clone();
    route_b_runtime.active_profile_id = profile.id;
    subagent_policy::reconcile_with_model_state(&mut route_b_runtime, Some(&startup.model_state));

    assert_eq!(
        startup
            .model_state
            .third_party_model_metadata
            .iter()
            .find(|model| model.slug == "vendor/model")
            .unwrap()
            .supported_reasoning_efforts,
        ["low"]
    );
    assert_eq!(
        route_b_runtime.subagent_roles[crate::config::SUBAGENT_ROLE_WORKER].reasoning_effort,
        "low"
    );
    assert_eq!(config, saved);
}
