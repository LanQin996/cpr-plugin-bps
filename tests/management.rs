//! 通过真实插件会话验证空配置页面、自动建号、配置发布及操作重试

use cpr_plugin_bps::{PLUGIN_ID, manifest};
use gateway_plugin_sdk::{
    CallContext, Frame, Handshake, Message, PROTOCOL_VERSION, Stage,
    call::management::{ManagementRequest, ManagementResponse},
    client::{read_frame, write_frame},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::{io::AsyncWrite, process::Command};

async fn send(writer: &mut (impl AsyncWrite + Unpin), message: Message, payload: Vec<u8>) {
    write_frame(writer, &Frame { message, payload })
        .await
        .unwrap();
}

#[tokio::test]
async fn import_from_empty_installation_creates_account_and_updates_only_its_instance() {
    tokio::time::timeout(Duration::from_secs(15),async {
        let mut child=Command::new(env!("CARGO_BIN_EXE_cpr-plugin-bps")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let mut writer=child.stdin.take().unwrap();let mut reader=child.stdout.take().unwrap();
        send(&mut writer,Message::Hello{handshake:Handshake{protocol_version:PROTOCOL_VERSION,artifact_sha256:"a".repeat(64),plugin_id:PLUGIN_ID.into(),instance_id:"manage-instance".into(),generation:1,incarnation:"manage-process".into(),configuration:json!({}),contributes:manifest().unwrap().contributes}},vec![]).await;
        assert!(matches!(read_frame(&mut reader).await.unwrap().message,Message::Ready{..}));
        let mut state:BTreeMap<String,Value>=BTreeMap::new();let mut counter=0;let mut created=0;let mut updates=0;let mut published_config=Value::Null;
        let body=json!({"operationId":"c82ce56d-435a-4ffa-874e-7f10f72146d9","label":"我的账号","models":["gpt-6-astra"],"oauth":json!({"access_token":"test-managed-oauth","expires_at":cpr_plugin_bps::auth::now()+3600}).to_string()});
        for (id,path,payload) in [
            (1,"api/snapshot",None),(3,"api/onboarding/import",Some(body.clone())),
            (5,"api/onboarding/poll",Some(json!({"operationId":body["operationId"]}))),
        ] {
            let request=ManagementRequest{method:if payload.is_some(){"POST".into()}else{"GET".into()},path:path.into(),query:String::new(),content_type:payload.as_ref().map(|_|"application/json".into()),headers:vec![gateway_plugin_sdk::call::middleware::MiddlewareHeader{name:"authorization".into(),value:b"Bearer fixture-admin-session".to_vec()}]};
            let context=CallContext{request:None,call_id:id,instance_id:"manage-instance".into(),generation:1,incarnation:"manage-process".into(),stage:Stage::Management,timeout_ms:10000,resource_stream:false,resource_scope_id:format!("scope-{id}"),request_id:None,attempt_id:None,account_id:None,credential_revision:None};
            send(&mut writer,Message::Call{id,method:"management.handle".into(),context,params:serde_json::to_value(request).unwrap()},payload.map(|v|v.to_string().into_bytes()).unwrap_or_default()).await;
            loop {
                let frame=read_frame(&mut reader).await.unwrap();
                match frame.message {
                    Message::Suspend{..}|Message::CallbackConsumed{..}=>{},
                    Message::Resume{id,epoch}=>send(&mut writer,Message::Resumed{id,epoch},vec![]).await,
                    Message::Callback{id,method,params,..}=>{
                        let (result,payload)=match method.as_str(){
                            "host.auth.list"=>(json!({}),json!({"accounts":[],"next_cursor":null}).to_string().into_bytes()),
                            "host.data.accounts.list"=>(json!({}),json!({"schema_version":1,"accounts":[],"next_cursor":null}).to_string().into_bytes()),
                            "host.state.get"=>(json!({"record":state.get(params["key"].as_str().unwrap())}),vec![]),
                            "host.state.put"=>{
                                let key=params["key"].as_str().unwrap();assert_eq!(params["expected_version"],state.get(key).map(|r|r["version"].clone()).unwrap_or(Value::Null));
                                counter+=1;state.insert(key.into(),json!({"value":params["value"],"version":counter,"schema_version":1}));(json!({"version":counter}),vec![])
                            }
                            "host.state.delete"=>{let key=params["key"].as_str().unwrap();assert_eq!(params["expected_version"],state[key]["version"]);state.remove(key);(json!({"deleted":true}),vec![])},
                            "host.auth.save"=>{
                                assert_eq!(params,json!({}));let request:Value=serde_json::from_slice(&frame.payload).unwrap();assert_eq!(request["action"],"create");assert_eq!(request["provider_id"],"openai");
                                assert_eq!(request["facts"]["authentication_kind"],"api_key");assert_eq!(request["facts"]["material"]["base_url"],"https://bps-plugin.invalid/");assert!(!request.to_string().contains("test-managed-oauth"));
                                created+=1;(json!({}),json!({"account_id":"auto-acct","credential_revision":1}).to_string().into_bytes())
                            }
                            "host.http.dispatch"=>{
                                assert_eq!(params["headers"][0]["name"],"authorization");
                                let uri=params["uri"].as_str().unwrap();
                                let data=if uri=="/api/admin/plugins/instances" {
                                    assert_eq!(params["method"],"GET");
                                    // 第二次调用模拟第一次结果丢失，旧进程仍未刷新，发布重试不得再建号
                                    json!([{"id":"another-instance","name":"Other","configuration":{}},{"id":"manage-instance","name":"BPS","artifactSha256":"abc","enabled":true,"configuration":{},"revision":9,"bindings":[{"contribution":"local.bps.upstream-adapter","stage":"upstream","order":7,"failurePolicy":"reject","providerIds":[],"models":[],"clientKeyIds":[],"accountGroupIds":[],"identityBindings":[]}]}])
                                }else if uri.starts_with("/api/admin/account-groups/options?") {
                                    json!({"items":[],"page":{"totalPages":0}})
                                }else{
                                    assert_eq!(uri,"/api/admin/plugins/instances/update");let request:Value=serde_json::from_slice(&frame.payload).unwrap();assert_eq!(request["id"],"manage-instance");assert_eq!(request["instance"]["expectedRevision"],9);assert_eq!(request["instance"]["bindings"][0]["order"],7);
                                    published_config=request["instance"]["configuration"].clone();assert_eq!(published_config["accounts"][0]["accountId"],"auto-acct");assert_eq!(published_config["accounts"][0]["label"],"我的账号");assert!(!request.to_string().contains("test-managed-oauth"));updates+=1;json!({"id":"manage-instance","configRevision":10})
                                };
                                let conflict=uri.ends_with("/update") && updates==1;
                                let status=if conflict {409}else{200};
                                let body=json!({"code":status,"message":if conflict{"配置发布冲突，请继续接入"}else{"OK"},"data":data}).to_string().into_bytes();
                                (json!({"status":status,"version":"HTTP/1.1","headers":[],"body":{"kind":"handle","handle":format!("admin-{id}")},"response":null,"session":false}),{
                                    state.insert(format!("admin-{id}"),json!({"body":String::from_utf8(body).unwrap(),"sent":false}));vec![]
                                })
                            }
                            "host.http.body_read"=>{
                                let record=state.get_mut(params["handle"].as_str().unwrap()).unwrap();
                                if record["sent"]==true {(json!({"eof":true,"trailers":null}),vec![])} else {record["sent"]=json!(true);(json!({"eof":false,"trailers":null}),record["body"].as_str().unwrap().as_bytes().to_vec())}
                            }
                            _=>panic!("Unexpected management callback: {method}"),
                        };
                        send(&mut writer,Message::Result{id,result},payload).await;
                    }
                    Message::Result{result,..}=>{
                        let response:ManagementResponse=serde_json::from_value(result).unwrap();let data:Value=serde_json::from_slice(&frame.payload).unwrap();
                        assert!(!data.to_string().contains("test-managed-oauth"));
                        if path=="api/onboarding/import" {assert_eq!(response.status,409);assert_eq!(created,1);break;}
                        assert_eq!(response.status,200,"{}",data);
                        if path=="api/snapshot" {assert_eq!(data["accounts"],json!([]));assert_eq!(data["onboarding"],Value::Null);}else{assert_eq!(data["status"],"authorized");}
                        break;
                    }
                    Message::Error{error,..}=>panic!("Management fault: {}",error.message),
                    message=>panic!("Unexpected frame: {message:?}"),
                }
            }
        }
        assert_eq!(created,1);assert_eq!(updates,2);assert_eq!(published_config["accounts"].as_array().unwrap().len(),1);
        assert!(state.values().any(|record|record["value"]["access_token"]=="test-managed-oauth"));
        send(&mut writer,Message::Shutdown,vec![]).await;assert!(child.wait().await.unwrap().success());
    }).await.expect("Management fixture timed out");
}
