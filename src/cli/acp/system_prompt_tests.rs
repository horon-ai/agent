//! A request that carries a `systemPrompt` is refused on v1 and v2 before a
//! session is created, loaded or resumed: this adapter advertises no
//! `systemPrompt` capability and applies none, so the only honest answer is
//! `-32602` with nothing sent to the daemon. An omitted key and `null` mean
//! the same thing in both schemas and keep working as before.

use super::tests::connection_core::*;
use super::*;
use tokio::sync::mpsc;

const CWD: &str = "/work";

fn append_prompt() -> Value {
    json!({"mode":"append","content":[{"type":"text","text":"x"}]})
}

fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

async fn v1_client() -> (Client, mpsc::UnboundedReceiver<FakeDaemon>) {
    let (connector, daemons) = scripted_daemon();
    let mut client = start_client_with(Some(connector));
    client
        .send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}}))
        .await;
    let response = client.recv().await;
    assert_eq!(response["result"]["protocolVersion"], 1, "{response}");
    (client, daemons)
}

async fn v2_client() -> (Client, mpsc::UnboundedReceiver<FakeDaemon>) {
    let (connector, daemons) = scripted_daemon();
    let mut client = start_client_with(Some(connector));
    client.initialize_v2().await;
    (client, daemons)
}

/// Assert one answer is the `-32602` refusal that names `systemPrompt`.
#[track_caller]
fn assert_refused(method: &str, params: &Value, response: &Value) {
    assert_eq!(
        response["error"]["code"], JSONRPC_INVALID_PARAMS,
        "{method} {params}: {response}"
    );
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("systemPrompt"),
        "{method} {params}: the message names it: {response}"
    );
    assert!(
        message.contains("cannot apply"),
        "{method} {params}: the message says why: {response}"
    );
}

/// v1 `session/new`, `session/load` and `session/resume`, in that order, all
/// refuse a `systemPrompt` and none of them opens a daemon connection.
#[tokio::test]
async fn v1_lifecycle_methods_refuse_a_system_prompt() {
    let (mut client, mut daemons) = v1_client().await;
    let cases = [
        ("session/new", json!({"cwd":CWD,"systemPrompt":append_prompt()})),
        (
            "session/load",
            json!({"sessionId":"s1","cwd":CWD,"systemPrompt":append_prompt()}),
        ),
        (
            "session/resume",
            json!({"sessionId":"s1","cwd":CWD,"systemPrompt":append_prompt()}),
        ),
    ];
    for (index, (method, params)) in cases.iter().enumerate() {
        let id = json!(index + 1);
        client.send(request(id.clone(), method, params.clone())).await;
        let (before, response) = client.recv_response(id).await;
        assert!(before.is_empty(), "{method}: nothing precedes the answer: {before:?}");
        assert_refused(method, params, &response);
    }
    assert!(client.barrier("v1-system-prompt").await.is_empty());
    assert!(
        daemons.try_recv().is_err(),
        "a refused systemPrompt opens no daemon connection"
    );
    client.close_stdin().await;
}

/// v2 `session/new` and `session/resume` refuse it the same way.
#[tokio::test]
async fn v2_lifecycle_methods_refuse_a_system_prompt() {
    let (mut client, mut daemons) = v2_client().await;
    let cases = [
        ("session/new", json!({"cwd":CWD,"systemPrompt":append_prompt()})),
        (
            "session/resume",
            json!({"sessionId":"s1","cwd":CWD,"mcpServers":[],"systemPrompt":append_prompt()}),
        ),
    ];
    for (index, (method, params)) in cases.iter().enumerate() {
        let id = json!(index + 1);
        client.send(request(id.clone(), method, params.clone())).await;
        let (before, response) = client.recv_response(id).await;
        assert!(before.is_empty(), "{method}: nothing precedes the answer: {before:?}");
        assert_refused(method, params, &response);
    }
    assert!(client.barrier("v2-system-prompt").await.is_empty());
    assert!(
        daemons.try_recv().is_err(),
        "a refused systemPrompt opens no daemon connection"
    );
    client.close_stdin().await;
}

/// A value the schema does not describe is refused just as a well-formed one
/// is: the adapter applies no form of `systemPrompt`, so it cannot honour any.
#[tokio::test]
async fn a_malformed_system_prompt_is_refused_the_same_way() {
    let (mut client, mut daemons) = v1_client().await;
    let values = [
        json!("append x"),
        json!([{"mode":"append"}]),
        json!(7),
        json!(true),
        json!({"mode":"append","content":"x"}),
        json!({"content":[{"type":"text","text":"x"}]}),
    ];
    for (index, value) in values.iter().enumerate() {
        let id = json!(index + 1);
        let params = json!({"cwd":CWD,"systemPrompt":value});
        client.send(request(id.clone(), "session/new", params.clone())).await;
        let (before, response) = client.recv_response(id).await;
        assert!(before.is_empty(), "{value}: {before:?}");
        assert_refused("session/new", &params, &response);
    }
    assert!(client.barrier("v1-system-prompt-values").await.is_empty());
    assert!(daemons.try_recv().is_err(), "{values:?} opened a daemon connection");
    client.close_stdin().await;
}

/// Omitted and `null` are the same request in both schemas and stay accepted
/// by the v1 parser.
#[test]
fn an_omitted_or_null_system_prompt_is_not_refused() {
    for params in [json!({"cwd":CWD}), json!({"cwd":CWD,"systemPrompt":null})] {
        let setup = session_setup_from_params(&params).unwrap_or_else(|err| panic!("{params}: {err}"));
        assert_eq!(setup.cwd, PathBuf::from(CWD), "{params}");
    }
}

/// The v2 handshake of one `session/new` whose setup the daemon accepts,
/// returning the response line.
async fn v2_new_session(params: Value) -> Value {
    let (mut client, mut daemons) = v2_client().await;
    client.send(request(json!(1), "session/new", params.clone())).await;
    let mut daemon = within("the session/new connection", daemons.recv())
        .await
        .unwrap_or_else(|| panic!("{params} reached no daemon"));
    let subscribe = daemon.recv_type("subscribe").await;
    daemon.emit(json!({"type":"done","id":subscribe["id"]})).await;
    let get_history = daemon.recv_type("get_history").await;
    daemon
        .emit(json!({"type":"history","id":get_history["id"],"session_id":"new-1",
            "provider_model":"m0","available_models":["m0","m1"],"messages":[],
            "acp_protocol_version":2}))
        .await;
    let mode = daemon.recv_type("set_session_approval_mode").await;
    daemon.emit(json!({"type":"ack","id":mode["id"]})).await;
    let (_, response) = client.recv_response(json!(1)).await;
    client.close_stdin().await;
    response
}

/// v2 keeps creating the session when the key is absent or `null`.
#[tokio::test]
async fn v2_session_new_creates_the_session_for_an_omitted_or_null_system_prompt() {
    for params in [json!({"cwd":CWD}), json!({"cwd":CWD,"systemPrompt":null})] {
        let response = v2_new_session(params.clone()).await;
        assert_eq!(response["result"]["sessionId"], "new-1", "{params}: {response}");
    }
}

/// v1 `initialize` advertises `sessionCapabilities` without a `systemPrompt`
/// entry, so a client is told not to send one.
#[test]
fn v1_initialize_advertises_no_system_prompt_capability() {
    for profile in [AcpProfile::Standard, AcpProfile::Extended] {
        let result = initialize_result(profile);
        let capabilities = &result["agentCapabilities"]["sessionCapabilities"];
        assert!(
            capabilities.get("systemPrompt").is_none(),
            "{profile:?}: {capabilities}"
        );
        assert!(
            capabilities.get("resume").is_some(),
            "{profile:?}: nothing else moved: {capabilities}"
        );
    }
}

/// v2 `initialize` puts the session capabilities at `capabilities.session` and
/// has no `systemPrompt` there either.
#[test]
fn v2_initialize_advertises_no_system_prompt_capability() {
    for profile in [AcpProfile::Standard, AcpProfile::Extended] {
        for http in [true, false] {
            let response = v2::shapes::initialize_response(profile, http);
            let session = &response["capabilities"]["session"];
            assert!(
                session.get("systemPrompt").is_none(),
                "{profile:?} http={http}: {session}"
            );
            assert!(
                session.get("delete").is_some(),
                "{profile:?} http={http}: nothing else moved: {session}"
            );
        }
    }
}
