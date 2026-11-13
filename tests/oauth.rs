//! 通过 SDK 会话模拟设备授权、PKCE、私有状态 CAS 与导入退出

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use cpr_plugin_bps::{PLUGIN_ID, manifest};
use gateway_plugin_sdk::{
    CallContext, Frame, Handshake, Message, PROTOCOL_VERSION, Stage,
    call::management::{CommandInvocation, CommandResult, CommandValue},
    client::{read_frame, write_frame},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::{io::AsyncWrite, process::Command};

async fn send(writer: &mut (impl AsyncWrite + Unpin), message: Message, payload: Vec<u8>) {
    write_frame(writer, &Frame { message, payload })
        .await
        .unwrap();
}

#[tokio::test]
async fn device_authorization_and_import_persist_without_exposing_tokens() {
    tokio::time::timeout(Duration::from_secs(15),async {
        let mut child=Command::new(env!("CARGO_BIN_EXE_cpr-plugin-bps")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let mut writer=child.stdin.take().unwrap(); let mut reader=child.stdout.take().unwrap();
        send(&mut writer,Message::Hello{handshake:Handshake{protocol_version:PROTOCOL_VERSION,artifact_sha256:"a".repeat(64),plugin_id:PLUGIN_ID.into(),instance_id:"oauth-test".into(),generation:1,incarnation:"oauth-process".into(),configuration:json!({"accounts":[{"accountId":"acct1","models":["m"]}]}),contributes:manifest().unwrap().contributes}},vec![]).await;
        assert!(matches!(read_frame(&mut reader).await.unwrap().message,Message::Ready{..}));
        let mut state:BTreeMap<String,Value>=BTreeMap::new();
        let mut bodies:BTreeMap<String,Vec<u8>>=BTreeMap::new();
        let verifier="x".repeat(43);
        let mut token_calls=0;
        let mut counter=0;
        for (id,name,args) in [
            (1,"login-start",BTreeMap::new()),(3,"login-poll",BTreeMap::new()),(5,"status",BTreeMap::new()),
            (7,"import-oauth",BTreeMap::from([("oauth".into(),CommandValue::String(json!({"access_token":"replacement-fixture","expires_at":cpr_plugin_bps::auth::now()+3600}).to_string()))])),
            (9,"logout",BTreeMap::new()),(11,"status",BTreeMap::new()),
        ] {
            let invocation=CommandInvocation{name:name.into(),arguments:args};
            let context=CallContext{request:None,call_id:id,instance_id:"oauth-test".into(),generation:1,incarnation:"oauth-process".into(),stage:Stage::CommandLine,timeout_ms:5000,resource_stream:false,resource_scope_id:format!("scope-{id}"),request_id:None,attempt_id:None,account_id:None,credential_revision:None};
            send(&mut writer,Message::Call{id,method:"command_line.execute".into(),context,params:json!({})},serde_json::to_vec(&invocation).unwrap()).await;
            loop {
                let frame=read_frame(&mut reader).await.unwrap();
                match frame.message {
                    Message::Suspend{..}|Message::CallbackConsumed{..}=>{},
                    Message::Resume{id,epoch}=>send(&mut writer,Message::Resumed{id,epoch},vec![]).await,
                    Message::Callback{id,method,params,..}=>{
                        let (result,payload)=match method.as_str() {
                            "host.state.get"=>(json!({"record":state.get(params["key"].as_str().unwrap())}),vec![]),
                            "host.state.put"=>{
                                let key=params["key"].as_str().unwrap();
                                assert_eq!(params["expected_version"],state.get(key).map(|r|r["version"].clone()).unwrap_or(Value::Null));
                                counter+=1;
                                state.insert(key.into(),json!({"value":params["value"],"version":counter,"schema_version":1}));
                                (json!({"version":counter}),vec![])
                            }
                            "host.state.delete"=>{
                                let key=params["key"].as_str().unwrap();
                                assert_eq!(params["expected_version"],state[key]["version"]);
                                state.remove(key); (json!({"deleted":true}),vec![])
                            }
                            "host.http.do_stream"=>{
                                let url=params["url"].as_str().unwrap();
                                assert!(url.starts_with("https://auth.openai.com/"));
                                let data=if url.ends_with("usercode") {json!({"device_auth_id":"device1","user_code":"CODE-1","interval":1})}
                                else if url.ends_with("deviceauth/token") {json!({"authorization_code":"grant1","code_verifier":verifier,"code_challenge":URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))})}
                                else {
                                    assert!(url.ends_with("/oauth/token?unified=true"));
                                    let form:String=String::from_utf8(frame.payload).unwrap();
                                    assert!(form.contains("grant_type=authorization_code")); token_calls+=1;
                                    json!({"access_token":"device-fixture-token","token_type":"Bearer","expires_in":3600,"refresh_token":"refresh-fixture"})
                                };
                                let stream=format!("http-{}",bodies.len()); bodies.insert(stream.clone(),data.to_string().into_bytes());
                                (json!({"status":200,"http_version":"HTTP/1.1","headers":[],"stream":stream,"trailers":[]}),vec![])
                            }
                            "host.http.stream_read"=>{
                                let body=bodies.get_mut(params["stream"].as_str().unwrap()).unwrap();
                                if body.is_empty() {(json!({"eof":true,"trailers":[]}),vec![])}
                                else {(json!({"eof":false,"trailers":[]}),std::mem::take(body))}
                            }
                            method=>panic!("Unexpected OAuth callback: {method}"),
                        };
                        send(&mut writer,Message::Result{id,result},payload).await;
                    }
                    Message::Result{..}=>{
                        let result:CommandResult=serde_json::from_slice(&frame.payload).unwrap();
                        assert_eq!(result.exit_code,0);
                        assert!(!result.stdout.contains("fixture-token")); assert!(!result.stdout.contains("refresh-fixture")); assert!(!result.stdout.contains("replacement-fixture"));
                        if name=="login-start" {assert!(result.stdout.contains("CODE-1"));}
                        if name=="logout" {assert!(state.is_empty());}
                        if id==11 {assert!(result.stdout.contains("尚未配置"));}
                        break;
                    }
                    Message::Error{error,..}=>panic!("OAuth command failed: {}",error.message),
                    message=>panic!("Unexpected OAuth frame: {message:?}"),
                }
            }
            if name=="login-start" {tokio::time::sleep(Duration::from_millis(1100)).await;}
        }
        assert_eq!(token_calls,1);
        send(&mut writer,Message::Shutdown,vec![]).await; assert!(child.wait().await.unwrap().success());
    }).await.expect("OAuth fixture timed out");
}
