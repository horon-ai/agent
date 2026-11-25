use super::Config;

struct HomeEnvGuard(Option<std::ffi::OsString>);

impl HomeEnvGuard {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", path);
        Config::invalidate_cache();
        Self(prev)
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(prev) => crate::env::set_var("HORON_HOME", prev),
            None => crate::env::remove_var("HORON_HOME"),
        }
        Config::invalidate_cache();
    }
}

#[test]
fn check_strict_rejects_unknown_keys_and_missing_default_provider() {
    let _guard = crate::storage::lock_test_env();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let _home = HomeEnvGuard::set(dir.path());

    let path = Config::path().expect("config path");
    std::fs::write(&path, "[features]\nauto_pokke = true\n").expect("write typo key");
    let err = Config::check_strict().expect_err("unknown [features] key must fail check_strict");
    assert!(err.to_string().contains("features.auto_pokke"), "{err}");

    std::fs::write(&path, "[provider]\ndefault_provider = \"nonexistent\"\n").expect("write missing default_provider");
    let err = Config::check_strict().expect_err("missing default_provider must fail check_strict");
    assert!(err.to_string().contains("default_provider"), "{err}");

    std::fs::write(
        &path,
        "[providers.mock]\ntype = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:8080/v1\"\nbogus = 1\n",
    )
    .expect("write typo provider key");
    let err = Config::check_strict().expect_err("unknown [providers.mock] key must fail check_strict");
    assert!(err.to_string().contains("providers.mock.bogus"), "{err}");

    std::fs::write(
        &path,
        "[features]\nauto_poke = false\n\n[hosting]\nallow_shared_children = false\n\n[harness_api]\ninsecure_behind_proxy = false\n\n[desktop.voice]\nkept = true\n\n[provider]\ndefault_provider = \"mock\"\n\n[providers.mock]\ntype = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:8080/v1\"\nauth = \"none\"\ndefault_model = \"mock-model\"\nmodels = [{ id = \"mock-model\" }]\n\n[providers.mock.roles]\naux = \"mock-model\"\n",
    )
    .expect("write valid config");
    let valid = Config::check_strict().expect("valid config with hosting/harness_api/desktop/roles must pass");
    assert_eq!(valid.provider.default_provider.as_deref(), Some("mock"));
}
