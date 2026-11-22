//! Auxiliary roles name a model inside the active profile. They never select a
//! second profile, endpoint or credential, so a role value that looks like another
//! profile's model is still sent to the active profile's endpoint.

use super::*;
use crate::config::{ModelRole, resolve_role_model};

#[test]
fn roles_naming_another_profiles_model_call_only_the_active_endpoint() {
    with_clean_provider_test_env(|| {
        let active = RecordingServer::start();
        let other = RecordingServer::start();
        let (active_url, other_url) = (active.base_url.clone(), other.base_url.clone());
        write_provider_config(&format!(
            r#"
[providers.alpha]
type = "openai-compatible"
base_url = "{active_url}"
auth = "none"
default_model = "alpha-model"

[providers.alpha.roles]
aux = "beta/aux-model"
judge = "beta/judge-model"
plan = "beta/plan-model"

[providers.paint]
type = "openai-responses"
base_url = "{active_url}"
auth = "none"
default_model = "paint-model"

[providers.paint.roles]
image = "beta/image-model"

[providers.beta]
type = "openai-compatible"
base_url = "{other_url}"
auth = "none"
default_model = "beta-model"
"#
        ));

        // Every role resolves to the model id the profile wrote, never to a profile.
        let config = crate::config::config();
        let alpha = &config.providers["alpha"];
        for (role, expected) in [
            (ModelRole::Aux, "beta/aux-model"),
            (ModelRole::Judge, "beta/judge-model"),
            (ModelRole::Plan, "beta/plan-model"),
        ] {
            let resolved = resolve_role_model(
                role,
                None,
                &alpha.model_roles.roles,
                alpha.default_model.as_deref(),
                None,
            )
            .expect("the role resolves inside its profile");
            assert_eq!(resolved, expected);
        }
        let endpoint = external::image_endpoint("paint")
            .expect("the Responses wire accepts image generation")
            .expect("the profile names an image model");
        assert_eq!(
            endpoint.base_url, active_url,
            "the image route stays on its own profile"
        );
        assert_eq!(external::image_model("paint").as_deref(), Some("beta/image-model"));

        // A request made with that model reaches the active endpoint only.
        let runtime = enter_test_runtime();
        let provider = MultiProvider::from_named_provider_profile("alpha").expect("the active profile builds");
        let handle = provider
            .execution_provider()
            .expect("the active profile installs its runtime");
        handle
            .set_model("beta/aux-model")
            .expect("a profile without a catalog accepts the model id");
        let sent = runtime.block_on(async { handle.complete(&[Message::user("ping")], &[], "", None).await });
        assert!(sent.is_ok(), "the active endpoint answers");
        assert_eq!(active.request_count(), 1);
        assert_eq!(active.last_request_body()["model"], "beta/aux-model");
        assert_eq!(
            other.request_count(),
            0,
            "the other profile's endpoint is never contacted"
        );
    });
}
