//! 经真实插件子进程和公开 SDK 帧验证原生 WS 回调、流控、错误与取消

use cpr_plugin_bps::{PLUGIN_ID, manifest};
use gateway_plugin_sdk::{
    CallContext, Frame, Handshake, Message, PROTOCOL_VERSION, Stage,
    call::{
        model::{CanonicalEvent, WirePayload},
        upstream_adapter::{BuiltinProvider, UpstreamAdapterEvent, UpstreamAdapterRequest},
    },
    client::{read_frame, write_frame},
};
use serde_json::{Value, json};
use std::{collections::VecDeque, process::Stdio, time::Duration};
use tokio::{io::AsyncWrite, process::Command};

async fn send(writer: &mut (impl AsyncWrite + Unpin), message: Message, payload: Vec<u8>) {
    write_frame(writer, &Frame { message, payload })
        .await
        .unwrap();
}
fn context(id: u64, stage: Stage) -> CallContext {
    CallContext {
        request: None,
        call_id: id,
        instance_id: "test-instance".into(),
        generation: 1,
        incarnation: "test-process".into(),
        stage,
        timeout_ms: 5000,
        resource_stream: true,
        resource_scope_id: format!("scope-{id}"),
        request_id: Some("request1".into()),
        attempt_id: Some("attempt1".into()),
        account_id: Some("acct1".into()),
        credential_revision: Some(1),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    Text,
    Json,
    Tool,
    Failure,
    Disconnected,
    Rejected,
    Cancelled,
    Host,
    HostChanged,
    HostMany,
}

async fn run(scenario: Scenario) -> Vec<UpstreamAdapterEvent> {
    tokio::time::timeout(Duration::from_secs(10),async move {
        let mut child=Command::new(env!("CARGO_BIN_EXE_cpr-plugin-bps")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let mut writer=child.stdin.take().unwrap(); let mut reader=child.stdout.take().unwrap();
        let host_mode=matches!(scenario,Scenario::Host|Scenario::HostChanged|Scenario::HostMany);
        let config=if scenario==Scenario::HostMany {
            json!({"accounts":(0..32).map(|i|json!({"accountId":format!("acct{i}"),"models":["m"],"credentialSource":"host"})).collect::<Vec<_>>()})
        }else{json!({"accounts":[{"accountId":"acct1","models":["m"],"credentialSource":if host_mode{"host"}else{"independent"}}]})};
        send(&mut writer,Message::Hello{handshake:Handshake{protocol_version:PROTOCOL_VERSION,artifact_sha256:"a".repeat(64),plugin_id:PLUGIN_ID.into(),instance_id:"test-instance".into(),generation:1,incarnation:"test-process".into(),configuration:config,contributes:manifest().unwrap().contributes}},vec![]).await;
        assert!(matches!(read_frame(&mut reader).await.unwrap().message,Message::Ready{..}));
        let upstream=UpstreamAdapterRequest{adapter_id:"bps-0".into(),provider:BuiltinProvider::OpenAi,upstream_model:"m".into(),client_key_id:"key1".into(),account_id:"acct1".into(),credential_revision:1,protocol:"openai".into(),client_transport:"http_sse".into(),fast_mode:"default".into(),request_elapsed_ms:0,headers:vec![],continuation:None};
        let request=json!({"model":"public","input":"hello","stream":scenario!=Scenario::Json,"tools":[{"type":"function","name":"safe","parameters":{"type":"object"}}]});
        send(&mut writer,Message::Call{id:1,method:"upstream_adapter.execute".into(),context:context(1,Stage::Upstream),params:json!({})},upstream.encode(request.to_string().into_bytes()).unwrap()).await;
        send(&mut writer,Message::Credit{id:1,bytes:1024*1024,frames:64},vec![]).await;
        let tool=json!({"type":"function_call","id":"fc1","call_id":"c1","name":"run_officejs","arguments":json!({"code":json!({"name":"safe","arguments":{"x":1}}).to_string()}).to_string(),"status":"completed"});
        let mut input=VecDeque::from([
            json!({"type":"basispoints.response.heartbeat","private":"hidden"}),
            json!({"type":"response.created","response":{"id":"r1","model":"m","output":[]}}),
            json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"OK"}),
            json!({"type":"response.completed","response":{"id":"r1","model":"m","output":[{"type":"message","id":"msg1","role":"assistant","content":[{"type":"output_text","text":"OK"}]}],"usage":{"input_tokens":5,"output_tokens":1}}}),
        ]);
        if scenario==Scenario::Failure { input=VecDeque::from([json!({"type":"error","error":{"code":"request_rejected","message":"Fixture rejection"}})]); }
        if scenario==Scenario::Disconnected { input=VecDeque::from([json!({"type":"response.created","response":{"id":"r1","output":[]}})]); }
        if scenario==Scenario::Tool { input=VecDeque::from([json!({"type":"response.created","response":{"id":"r1","model":"m","output":[]}}),json!({"type":"response.output_item.done","output_index":0,"item":tool}),json!({"type":"response.completed","response":{"id":"r1","model":"m","output":[tool]}})]); }
        let mut events=vec![]; let mut sent=0; let mut closed=0; let mut reads=0;
        loop {
            let frame=read_frame(&mut reader).await.unwrap();
            match frame.message {
                Message::Suspend{..}|Message::CallbackConsumed{..}=>{},
                Message::Resume{id,epoch}=>send(&mut writer,Message::Resumed{id,epoch},vec![]).await,
                Message::Result{id:1,..}=>{},
                Message::Callback{id,method,params,..}=>{
                    let (result,payload)=match method.as_str() {
                        "host.state.get"=>{assert!(!host_mode,"Host credentials must not read independent OAuth state");(json!({"record":{"version":3,"schema_version":1,"value":{"access_token":"fixture-bps-token","account_id":"bps-account"}}}),vec![])},
                        "host.auth.get"=>{
                            assert!(host_mode);assert_eq!(params,json!({}));let request:Value=serde_json::from_slice(&frame.payload).unwrap();assert_eq!(request["account_id"],"acct1");
                            (json!({}),json!({"account_id":"acct1","provider_id":"openai","credential_revision":if scenario==Scenario::HostChanged{2}else{1},"facts":{"name":"Existing","authentication_kind":"oauth","material":{"schema_version":1,"access_token":"native-host-fixture","refresh_token":"must-not-be-used-by-plugin"},"email":null,"upstream_user_id":null,"upstream_account_id":"native-account","plan_type":null,"has_refresh_token":true,"access_token_expires_at_ms":(cpr_plugin_bps::auth::now()+3600)*1000,"next_refresh_at_ms":null}}).to_string().into_bytes())
                        }
                        "host.upstream.websocket.open"=>{
                            assert_eq!(params["path"],"basispoints/api/responses");
                            let query=params["query"].to_string(); assert!(!query.contains("fixture-bps-token"));
                            assert!(params["headers"].as_array().unwrap().iter().any(|header|header[0]=="sec-websocket-protocol"));
                            if host_mode {
                                let header=params["headers"].as_array().unwrap().iter().find(|header|header[0]=="authorization").unwrap();
                                let bytes:Vec<u8>=serde_json::from_value(header[1].clone()).unwrap();assert_eq!(bytes,b"Bearer native-host-fixture");
                            }
                            if scenario==Scenario::Rejected {(json!({"status":429,"http_version":"HTTP/1.1","headers":[],"stream":null,"trailers":[]}),json!({"error":{"code":"rate_limit_exceeded","message":"Limited"}}).to_string().into_bytes())}
                            else {(json!({"status":101,"http_version":"HTTP/1.1","headers":[],"stream":null,"trailers":[]}),vec![])}
                        }
                        "host.upstream.websocket.send"=>{
                            let body:Value=serde_json::from_slice(&frame.payload).unwrap();
                            assert_eq!(body["type"],"response.create"); assert_eq!(body["model"],"m"); assert_eq!(body["model_selection"],"explicit");
                            assert!(body.get("tools").is_none()); assert!(body["basispoints_resume_token"].as_str().unwrap().starts_with("resume_"));
                            sent+=1; (json!({}),vec![])
                        }
                        "host.upstream.websocket.read"=>{
                            reads+=1;
                            if scenario==Scenario::Cancelled {
                                send(&mut writer,Message::Cancel{id:1},vec![]).await;
                                continue;
                            }
                            match input.pop_front() {Some(event)=>(json!({"kind":"text","eof":false}),event.to_string().into_bytes()),None=>(json!({"kind":null,"eof":true}),vec![])}
                        }
                        "host.upstream.websocket.close"=>{closed+=1;(json!({}),vec![])},
                        _=>panic!("Unexpected host callback: {method}"),
                    };
                    send(&mut writer,Message::Result{id,result},payload).await;
                }
                Message::Stream{id:1,..}=>{
                    events.push(UpstreamAdapterEvent::decode(&frame.payload).unwrap());
                    send(&mut writer,Message::Credit{id:1,bytes:frame.payload.len() as u32,frames:1},vec![]).await;
                }
                Message::End{id:1,error}=>{
                    assert_eq!(error.is_some(),scenario==Scenario::Disconnected);
                    break;
                }
                Message::Cancelled{id:1} if scenario==Scenario::Cancelled=>break,
                Message::Error{error,..} if scenario==Scenario::HostChanged=>{assert_eq!(error.code,gateway_plugin_sdk::ErrorCode::Conflict);break;},
                Message::Error{error,..}=>panic!("Plugin fault: {}",error.message),
                message=>panic!("Unexpected plugin frame: {message:?}"),
            }
        }
        if matches!(scenario,Scenario::Rejected|Scenario::HostChanged) {assert_eq!(sent,0);} else {assert_eq!(sent,1);}
        if !matches!(scenario,Scenario::Rejected|Scenario::Cancelled|Scenario::Disconnected|Scenario::HostChanged) {assert_eq!(closed,1);}
        if scenario==Scenario::Cancelled {assert_eq!(reads,1);}
        send(&mut writer,Message::Shutdown,vec![]).await;
        assert!(child.wait().await.unwrap().success());
        events
    }).await.expect("Plugin session timed out")
}

#[tokio::test]
async fn native_ws_text_and_nonstream_json_use_managed_callbacks() {
    for scenario in [Scenario::Text, Scenario::Json] {
        let events = run(scenario).await;
        assert_eq!(
            events
                .iter()
                .flat_map(|e| &e.event.facts)
                .filter(|f| matches!(f, CanonicalEvent::Completed { .. }))
                .count(),
            1
        );
        assert!(events.iter().any(|e| {
            e.event
                .facts
                .iter()
                .any(|f| matches!(f,CanonicalEvent::Usage{usage} if usage.input_tokens==Some(5)))
        }));
        if scenario == Scenario::Json {
            assert!(events.iter().any(|e| {
                e.event
                    .wire
                    .as_ref()
                    .is_some_and(|w| matches!(w.payload, WirePayload::RawJson { .. }))
            }));
        }
    }
}
#[tokio::test]
async fn native_ws_tool_transport_becomes_client_function() {
    let events = run(Scenario::Tool).await;
    assert!(
        events
            .iter()
            .flat_map(|e| &e.event.facts)
            .any(|f| matches!(f,CanonicalEvent::ToolCallDelta{name:Some(name),..} if name=="safe"))
    );
    for event in events {
        if let Some(wire) = event.event.wire
            && let WirePayload::Json { data, .. } = wire.payload
        {
            assert!(!data.to_string().contains("run_officejs"));
        }
    }
}
#[tokio::test]
async fn native_ws_failure_disconnect_and_rejection_never_complete() {
    for scenario in [
        Scenario::Failure,
        Scenario::Disconnected,
        Scenario::Rejected,
    ] {
        let events = run(scenario).await;
        assert!(
            !events
                .iter()
                .flat_map(|e| &e.event.facts)
                .any(|f| matches!(f, CanonicalEvent::Completed { .. }))
        );
        if scenario == Scenario::Rejected {
            assert_eq!(events[0].failure.as_ref().unwrap().status, Some(429));
            assert!(events[0].event.wire.is_some());
        }
    }
}
#[tokio::test]
async fn cancellation_stops_pending_ws_read_without_replay() {
    run(Scenario::Cancelled).await;
}

#[tokio::test]
async fn host_oauth_is_read_per_attempt_and_never_copied_or_refreshed_by_plugin() {
    let events = run(Scenario::Host).await;
    assert!(
        events
            .iter()
            .flat_map(|event| &event.event.facts)
            .any(|fact| matches!(fact, CanonicalEvent::Completed { .. }))
    );
    assert!(run(Scenario::HostChanged).await.is_empty());
    let events = run(Scenario::HostMany).await;
    assert!(
        events
            .iter()
            .flat_map(|event| &event.event.facts)
            .any(|fact| matches!(fact, CanonicalEvent::Completed { .. }))
    );
}
