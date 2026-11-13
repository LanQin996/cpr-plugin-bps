//! 原生 BPS 请求、工具、回放、结构化输出、认证和计量行为测试

use cpr_plugin_bps::{
    Config, auth,
    events::Events,
    manifest, plugin,
    protocol::{self, ReplayCache},
};
use gateway_plugin_sdk::call::{
    model::{CanonicalEvent, FinishReason, WirePayload},
    upstream_adapter::UpstreamFailureKind,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn prepared(extra: Value) -> protocol::Prepared {
    let mut request = json!({"model":"m","input":"hello"});
    request
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    protocol::prepare(
        &serde_json::to_vec(&request).unwrap(),
        "selected",
        "identity",
        Arc::new(Mutex::new(ReplayCache::default())),
    )
    .unwrap()
}
fn created() -> Value {
    json!({"type":"response.created","response":{"id":"r1","model":"m","output":[]}})
}
fn native(name: &str, args: Value) -> Value {
    json!({"type":"function_call","id":"fc_native","call_id":"call1","name":name,"arguments":args.to_string(),"status":"completed"})
}
fn completed(output: Vec<Value>) -> Value {
    json!({"type":"response.completed","response":{"id":"r1","model":"m","output":output,"usage":{"input_tokens":8,"output_tokens":3,"input_tokens_details":{"cached_tokens":2}}}})
}

#[test]
fn manifest_builds_native_account_scoped_capabilities() {
    let config = Config::parse(json!({"accounts":[{"accountId":"acct1","models":["m"]}]})).unwrap();
    let registration = config.registration();
    assert_eq!(registration.adapters[0].base_url, "https://bps.openai.com/");
    assert_eq!(registration.adapters[0].account_ids, vec!["acct1"]);
    assert_eq!(registration.adapters[0].transport.as_str(), "websocket");
    assert_eq!(config.catalog().models[0].adapter_id, "bps-0");
    assert!(plugin(config).is_ok());
    assert_eq!(manifest().unwrap().plugin_id().unwrap(), "local.bps");
    for config in [
        json!({"accounts":[]}),
        json!({"bridges":[]}),
        json!({"accounts":[{"accountId":"acct1","models":[]}]}),
        json!({"accounts":[{"accountId":"acct1","models":["m","m"]}]}),
        json!({"accounts":[{"accountId":"acct1","models":["m"]},{"accountId":"acct1","models":["m"]}]}),
    ] {
        assert!(Config::parse(config).is_err());
    }
}

#[test]
fn request_is_bps_whitelisted_with_explicit_model_and_stable_scoped_metadata() {
    let p = prepared(
        json!({"stream":false,"instructions":"Follow me","reasoning":{"effort":"ultra"},"temperature":0.1,"service_tier":"priority","tools":[{"type":"function","name":"run","parameters":{"type":"object"}}]}),
    );
    assert_eq!(p.body["model"], "selected");
    assert_eq!(p.body["reasoning_effort"], "xhigh");
    assert!(!p.streaming);
    assert_eq!(p.body["stream"], true);
    assert_eq!(p.body["store"], false);
    assert!(p.body.get("tools").is_none());
    assert!(p.body.get("temperature").is_none());
    assert!(p.body.get("service_tier").is_none());
    assert_eq!(p.body["input"][0]["role"], "developer");
    assert_eq!(p.body["input"][0]["content"][0]["text"], "Follow me");
    assert!(
        p.body["input"][1]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("run_officejs")
    );
    let a = prepared(json!({}));
    let b = prepared(json!({}));
    assert_eq!(a.body["metadata"], b.body["metadata"]);
    for (value, want) in [
        ("minimal", "low"),
        ("high", "high"),
        ("max", "xhigh"),
        ("", "medium"),
    ] {
        assert_eq!(protocol::normalize_effort(value).unwrap(), want);
    }
    assert!(protocol::normalize_effort("bogus").is_err());
}

#[test]
fn unsupported_request_semantics_fail_before_network() {
    for extra in [
        json!({"previous_response_id":"old"}),
        json!({"tool_choice":"required"}),
        json!({"background":true}),
        json!({"generate":false}),
        json!({"input":[{"type":"item_reference","id":"x"}]}),
        json!({"reasoning":{"mode":"other"}}),
    ] {
        let mut request = json!({"input":"hello"});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            protocol::prepare(
                &request.to_string().into_bytes(),
                "m",
                "id",
                Default::default()
            )
            .is_err()
        );
    }
}

#[test]
fn history_config_updates_do_not_override_explicit_effort() {
    let input = json!([{"type":"configuration_update","reasoning":{"effort":"high"}},{"role":"user","content":"hi"}]);
    assert_eq!(
        prepared(json!({"input":input})).body["reasoning_effort"],
        "high"
    );
    let p = prepared(json!({"input":input,"reasoning":{"effort":"low"}}));
    assert_eq!(p.body["reasoning_effort"], "low");
    assert!(
        !p.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "configuration_update")
    );
}

#[test]
fn https_images_pass_but_inline_data_and_file_ids_fail() {
    for image in [
        json!({"type":"image_url","image_url":{"url":"https://example.test/image.png","detail":"high"}}),
        json!({"type":"image","source":{"type":"url","url":"https://example.test/image.png"}}),
    ] {
        let p = prepared(json!({"input":[{"role":"user","content":[image]}]}));
        assert_eq!(p.body["input"][1]["content"][0]["type"], "input_image");
    }
    for image in [
        json!({"type":"input_image","image_url":"data:image/png;base64,AAAA"}),
        json!({"type":"input_image","image_url":"http://example.test/img"}),
        json!({"type":"input_image","image_url":"https://u:p@example.test/img"}),
        json!({"type":"input_image","file_id":"file1"}),
    ] {
        let request = json!({"input":[{"role":"user","content":[image]}]});
        assert!(
            protocol::prepare(
                request.to_string().as_bytes(),
                "m",
                "id",
                Default::default()
            )
            .is_err()
        );
    }
}

#[test]
fn native_tools_are_withheld_then_emitted_as_client_tools() {
    let mut p = prepared(
        json!({"tools":[{"type":"namespace","name":"functions","tools":[{"type":"function","name":"exec","parameters":{"type":"object"}}]}]}),
    );
    let item = native(
        "run_officejs",
        json!({"code":json!({"name":"functions.exec","arguments":{"cmd":"pwd"}}).to_string()}),
    );
    assert!(
        p.transform(json!({"type":"response.output_item.added","output_index":0,"item":item}))
            .unwrap()
            .is_empty()
    );
    assert!(
        p.transform(
            json!({"type":"response.function_call_arguments.delta","delta":"SECRET_NATIVE"})
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        p.transform(json!({"type":"response.output_item.done","output_index":0,"item":item}))
            .unwrap()
            .is_empty()
    );
    let events = p.transform(completed(vec![item])).unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0]["item"]["name"], "exec");
    assert_eq!(events[0]["item"]["namespace"], "functions");
    let args: Value = serde_json::from_str(events[1]["delta"].as_str().unwrap()).unwrap();
    assert_eq!(args["cmd"], "pwd");
    assert!(
        !events
            .iter()
            .any(|event| event.to_string().contains("run_officejs"))
    );
}

#[test]
fn raw_custom_tool_preserves_exact_patch_input() {
    let mut p = prepared(json!({"tools":[{"type":"custom","name":"apply_patch"}]}));
    let patch = "*** Begin Patch\n*** End Patch\n";
    let events = p
        .transform(completed(vec![native(
            "run_officejs",
            json!({"summary":"codex2api.custom/apply_patch","code":patch}),
        )]))
        .unwrap();
    assert_eq!(events[1]["delta"], patch);
    assert_eq!(events[0]["item"]["type"], "custom_tool_call");
}

#[test]
fn undeclared_and_ambiguous_tools_never_escape_validation() {
    let mut p = prepared(json!({"tools":[{"type":"function","name":"safe"}]}));
    assert!(
        p.transform(completed(vec![native("delete_workbook", json!({}))]))
            .is_err()
    );
    let mut p = prepared(json!({"tools":[{"type":"function","name":"safe"}]}));
    let code = "{\"name\":\"safe\",\"arguments\":{}} {\"name\":\"safe\",\"arguments\":{}}";
    assert!(
        p.transform(completed(vec![native(
            "run_officejs",
            json!({"code":code})
        )]))
        .is_err()
    );
}

#[test]
fn direct_calls_and_fenced_function_envelopes_are_supported() {
    for item in [
        native("safe", json!({"x":1})),
        native(
            "run_officejs",
            json!({"code":"```json\n{\"name\":\"safe\",\"arguments\":{\"x\":1}}\n```"}),
        ),
    ] {
        let mut p = prepared(json!({"tools":[{"type":"function","name":"safe"}]}));
        assert_eq!(
            p.transform(completed(vec![item])).unwrap()[0]["item"]["name"],
            "safe"
        );
    }
}

#[test]
fn replay_is_scoped_and_checks_complete_client_call_signature() {
    let mut cache = ReplayCache::default();
    let client =
        json!({"type":"function_call","call_id":"same","name":"safe","arguments":"{\"x\":1}"});
    cache.put("key1-acct1-session1", json!({"native":true}), &client);
    assert!(cache.get("other", "same", None).is_none());
    let mut altered = client.clone();
    altered["arguments"] = json!("{\"x\":2}");
    assert!(
        cache
            .get("key1-acct1-session1", "same", Some(&altered))
            .is_none()
    );
    assert!(
        cache
            .get("key1-acct1-session1", "same", Some(&client))
            .is_some()
    );
    for i in 0..1025 {
        let mut call = client.clone();
        call["call_id"] = json!(i.to_string());
        cache.put("scope", json!({}), &call);
    }
    assert!(cache.get("key1-acct1-session1", "same", None).is_none());
}

#[test]
fn complete_history_rebuilds_transport_after_process_restart() {
    let p = prepared(
        json!({"input":[{"role":"user","content":"start"},{"type":"custom_tool_call","call_id":"c","name":"patch","input":"abc"},{"type":"custom_tool_call_output","call_id":"c","id":"ctco_old","output":"OK"}]}),
    );
    let input = p.body["input"].as_array().unwrap();
    assert_eq!(input[2]["name"], "run_officejs");
    assert_eq!(input[3]["type"], "function_call_output");
    assert!(input[3]["id"].as_str().unwrap().starts_with("fc_"));
}

#[test]
fn structured_text_is_buffered_and_validated_before_reconstructed_events() {
    let format = json!({"format":{"type":"json_schema","name":"answer","strict":true,"schema":{"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}}});
    let mut p = prepared(json!({"text":format}));
    assert!(
        p.transform(json!({"type":"response.output_text.delta","delta":"invalid"}))
            .unwrap()
            .is_empty()
    );
    let item = json!({"type":"message","id":"msg1","role":"assistant","content":[{"type":"output_text","text":"{\"ok\":true}"}]});
    let events = p.transform(completed(vec![item.clone()])).unwrap();
    assert!(
        events
            .iter()
            .any(|v| v["type"] == "response.output_text.delta")
    );
    let mut p = prepared(json!({"text":format}));
    let mut bad = item;
    bad["content"][0]["text"] = json!("{\"ok\":\"not-bool\"}");
    assert!(p.transform(completed(vec![bad])).is_err());
    let request = json!({"input":"hi","text":{"format":{"type":"json_schema","name":"x","schema":{"$ref":"file:///secret"}}}});
    assert!(
        protocol::prepare(
            request.to_string().as_bytes(),
            "m",
            "id",
            Default::default()
        )
        .is_err()
    );
}

#[test]
fn controls_are_hidden_and_draining_is_not_automatically_resumed() {
    let mut p = prepared(json!({}));
    for kind in ["heartbeat", "metadata", "resume_token", "upstream_sent"] {
        assert!(
            p.transform(
                json!({"type":format!("basispoints.response.{kind}"),"secret":"do-not-leak"})
            )
            .unwrap()
            .is_empty()
        );
    }
    assert!(
        p.transform(json!({"type":"basispoints.response.server_draining"}))
            .is_err()
    );
    let events=p.transform(json!({"type":"response.created","basispoints_replay_cursor":"private","response":{"id":"r1","output":[]}})).unwrap();
    assert!(events[0].get("basispoints_replay_cursor").is_none());
}

#[test]
fn canonical_facts_include_usage_and_one_successful_termination() {
    let mut events = Events::new(true);
    events.convert(created()).unwrap();
    let delta=events.convert(json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"你好"})).unwrap().unwrap();
    assert!(
        matches!(&delta.event.facts[..],[CanonicalEvent::ContentAdded {..},CanonicalEvent::TextDelta{text,..}] if text=="你好")
    );
    let terminal = events.convert(completed(vec![])).unwrap().unwrap();
    assert!(terminal.event.facts.iter().any(|v|matches!(v,CanonicalEvent::Usage{usage} if usage.input_tokens==Some(8) && usage.cached_tokens==Some(2) && usage.reasoning_tokens.is_none())));
    assert!(terminal.event.facts.iter().any(|v| matches!(
        v,
        CanonicalEvent::Completed {
            reason: FinishReason::Stop,
            ..
        }
    )));
    assert!(events.convert(completed(vec![])).is_err());
}

#[test]
fn nonstreaming_returns_terminal_json_and_failures_never_claim_success() {
    let mut events = Events::new(false);
    assert!(
        events
            .convert(created())
            .unwrap()
            .unwrap()
            .event
            .wire
            .is_none()
    );
    let event = events.convert(completed(vec![])).unwrap().unwrap();
    assert!(matches!(
        event.event.wire.unwrap().payload,
        WirePayload::RawJson { .. }
    ));
    let mut events = Events::new(true);
    let event=events.convert(json!({"type":"response.failed","response":{"error":{"code":"request_rejected","message":"Denied"}}})).unwrap().unwrap();
    assert!(event.event.facts.is_empty());
    assert!(matches!(
        event.failure.unwrap().kind,
        UpstreamFailureKind::Protocol
    ));
}

#[test]
fn oauth_never_extends_expiry_or_accepts_api_keys() {
    let future = auth::now() + 3600;
    let parsed = auth::parse_credentials(
        &json!({"access_token":"fixture-token","expires_at":future,"account_id":"bps-acct"}),
    )
    .unwrap();
    assert_eq!(parsed.expires_at, Some(future));
    assert_eq!(parsed.account_id, "bps-acct");
    for doc in [
        json!({"OPENAI_API_KEY":"sk-fixture"}),
        json!({"access_token":"fixture","expires_at":auth::now()-1}),
        json!({"access_token":"fixture","expires_in":3600}),
        json!({"access_token":"fixture","tokens":{"access_token":"other"}}),
        json!({"access_token":"fixture","token_type":"Basic"}),
    ] {
        assert!(auth::parse_credentials(&doc).is_err());
    }
}

#[test]
fn native_plan_statuses_are_normalized_against_the_client_schema() {
    let schema = json!({"type":"object","properties":{"plan":{"type":"array","items":{"type":"object","properties":{"step":{"type":"string"},"status":{"enum":["pending","in_progress","completed"]}},"required":["step","status"],"additionalProperties":false}}},"required":["plan"],"additionalProperties":false});
    let mut p =
        prepared(json!({"tools":[{"type":"function","name":"update_plan","parameters":schema}]}));
    let events=p.transform(completed(vec![native("update_plan",json!({"plan":[{"description":"Read","status":"done"},{"title":"Write","status":"active"}]}))])).unwrap();
    let args: Value = serde_json::from_str(events[1]["delta"].as_str().unwrap()).unwrap();
    assert_eq!(args["plan"][0], json!({"step":"Read","status":"completed"}));
    assert_eq!(args["plan"][1]["status"], "in_progress");
}

#[test]
fn missing_authoritative_tool_items_and_wrong_response_identity_fail_closed() {
    let mut p = prepared(json!({"tools":[{"type":"function","name":"safe"}]}));
    p.transform(json!({"type":"response.output_item.done","item":native("safe",json!({}))}))
        .unwrap();
    assert!(p.transform(completed(vec![])).is_err());
    let mut events = Events::new(true);
    events.convert(created()).unwrap();
    let wrong = json!({"type":"response.completed","response":{"id":"different","output":[]}});
    assert!(events.check_identity(&wrong).is_err());
    assert!(events.convert(wrong).is_err());
    assert!(!events.terminal());
}

#[test]
fn structured_schema_reference_names_in_instance_data_are_not_schema_refs() {
    let mut p = prepared(
        json!({"text":{"format":{"type":"json_schema","name":"refs","schema":{"type":"object","properties":{"$ref":{"type":"string"}},"required":["$ref"]}}}}),
    );
    let item = json!({"type":"message","id":"m","content":[{"type":"output_text","text":"{\"$ref\":\"https://example.test/not-fetched\"}"}]});
    assert!(p.transform(completed(vec![item])).is_ok());
}

#[test]
fn oauth_jwt_metadata_cannot_be_extended_by_a_later_declared_expiry() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let expiry = auth::now() + 300;
    let claims =
        json!({"exp":expiry,"https://api.openai.com/auth":{"chatgpt_account_id":"from-claims"}});
    let token = format!(
        "e30.{}.signature",
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let credentials = auth::parse_credentials(
        &json!({"access_token":token,"account_id":"","expires_at":expiry+3600}),
    )
    .unwrap();
    assert_eq!(credentials.expires_at, Some(expiry));
    assert_eq!(credentials.account_id, "from-claims");
}
