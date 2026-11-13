//! 验证宿主账号目录、无登录接管与取消接入只更新插件配置

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
fn account() -> Value {
    json!({"account_id":"native-account","provider_id":"openai","credential_revision":7,"name":"本体账号","email":"fixture@example.test","upstream_user_id":null,"upstream_account_id":null,"plan_type":null,"authentication_kind":"oauth","enabled":true,"credential_state":"ready","has_refresh_token":true,"access_token_expires_at_ms":null,"next_refresh_at_ms":null})
}

async fn run(detach: bool, group_mode: bool) {
    tokio::time::timeout(Duration::from_secs(10),async move {
        let mut child=Command::new(env!("CARGO_BIN_EXE_cpr-plugin-bps")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let mut writer=child.stdin.take().unwrap();let mut reader=child.stdout.take().unwrap();
        let native=json!({"accountId":"native-account","models":["m"],"credentialSource":"host"});
        let config=if detach{json!({"accounts":[native]})}else{json!({})};
        send(&mut writer,Message::Hello{handshake:Handshake{protocol_version:PROTOCOL_VERSION,artifact_sha256:"a".repeat(64),plugin_id:PLUGIN_ID.into(),instance_id:"host-mode".into(),generation:1,incarnation:"host-mode-process".into(),configuration:config.clone(),contributes:manifest().unwrap().contributes}},vec![]).await;
        assert!(matches!(read_frame(&mut reader).await.unwrap().message,Message::Ready{..}));
        let mut bodies:BTreeMap<String,Option<Vec<u8>>>=BTreeMap::new();let mut updates=0;
        let choice=if group_mode{json!({"groupIds":["group-a"],"accountIds":["b1","overlap"]})}else{json!({"accountId":"native-account"})};
        for (id,method,path,body) in [(1,"GET","api/snapshot",None),(3,"POST",if group_mode{"api/accounts/selection"}else if detach{"api/accounts/detach"}else{"api/accounts/use-host"},Some(choice))] {
            let headers=vec![gateway_plugin_sdk::call::middleware::MiddlewareHeader{name:"authorization".into(),value:b"Bearer fixture-admin".to_vec()}];
            let request=ManagementRequest{method:method.into(),path:path.into(),query:String::new(),content_type:body.as_ref().map(|_|"application/json".into()),headers};
            let context=CallContext{request:None,call_id:id,instance_id:"host-mode".into(),generation:1,incarnation:"host-mode-process".into(),stage:Stage::Management,timeout_ms:5000,resource_stream:false,resource_scope_id:format!("scope-{id}"),request_id:None,attempt_id:None,account_id:None,credential_revision:None};
            send(&mut writer,Message::Call{id,method:"management.handle".into(),context,params:serde_json::to_value(request).unwrap()},body.map(|body|body.to_string().into_bytes()).unwrap_or_default()).await;
            loop {
                let frame=read_frame(&mut reader).await.unwrap();
                match frame.message {
                    Message::Suspend{..}|Message::CallbackConsumed{..}=>{},
                    Message::Resume{id,epoch}=>send(&mut writer,Message::Resumed{id,epoch},vec![]).await,
                    Message::Callback{id,method,params,..}=>{
                        let (result,payload)=match method.as_str(){
                            "host.auth.list"=>{assert_eq!(params,json!({}));let mut accounts=vec![account()];if group_mode{for id in ["overlap","b1","b2"]{let mut item=account();item["account_id"]=json!(id);accounts.push(item);}}(json!({}),json!({"accounts":accounts,"next_cursor":null}).to_string().into_bytes())},
                            "host.data.accounts.list"=>{
                                let members=if group_mode{json!([{"account_id":"native-account","provider_id":"openai","name":"A","email":null,"group_ids":["group-a"],"enabled":true,"updated_at_ms":0},{"account_id":"overlap","provider_id":"openai","name":"AB","email":null,"group_ids":["group-a","group-b"],"enabled":true,"updated_at_ms":0},{"account_id":"b1","provider_id":"openai","name":"B1","email":null,"group_ids":["group-b"],"enabled":true,"updated_at_ms":0},{"account_id":"b2","provider_id":"openai","name":"B2","email":null,"group_ids":["group-b"],"enabled":true,"updated_at_ms":0}])}else{json!([])};
                                (json!({}),json!({"schema_version":1,"accounts":members,"next_cursor":null}).to_string().into_bytes())
                            },
                            "host.auth.get_runtime"=>{assert_eq!(params,json!({}));let data:Value=serde_json::from_slice(&frame.payload).unwrap();assert_eq!(data["account_id"],"native-account");(json!({}),account().to_string().into_bytes())},
                            "host.state.get"=>(json!({"record":null}),vec![]),
                            "host.http.dispatch"=>{
                                let uri=params["uri"].as_str().unwrap();let data=if uri=="/api/admin/plugins/instances" {
                                    json!([{"id":"host-mode","name":"BPS","artifactSha256":"artifact","enabled":true,"configuration":config,"revision":1,"bindings":[]}])
                                }else if uri.starts_with("/api/admin/account-groups/options?") {
                                    json!({"items":[{"id":"group-a","name":"A","enabled":true},{"id":"group-b","name":"B","enabled":true}],"page":{"totalPages":1}})
                                }else{
                                    assert_eq!(uri,"/api/admin/plugins/instances/update");let request:Value=serde_json::from_slice(&frame.payload).unwrap();assert_eq!(request["id"],"host-mode");assert_eq!(request["instance"]["expectedRevision"],1);
                                    let accounts=request["instance"]["configuration"]["accounts"].as_array().unwrap();
                                    if group_mode {
                                        let ids:std::collections::BTreeSet<_>=accounts.iter().map(|account|account["accountId"].as_str().unwrap()).collect();assert_eq!(ids,std::collections::BTreeSet::from(["native-account","overlap","b1"]));
                                        assert_eq!(request["instance"]["configuration"]["hostSelection"]["groupIds"],json!(["group-a"]));
                                        assert_eq!(request["instance"]["configuration"]["hostSelection"]["accountIds"],json!(["b1","overlap"]));
                                    }else if detach {assert!(accounts.is_empty());}else{assert_eq!(accounts.len(),1);assert_eq!(accounts[0]["accountId"],"native-account");assert_eq!(accounts[0]["credentialSource"],"host");assert_eq!(accounts[0]["models"],json!(["gpt-6-astra"]));}
                                    updates+=1;json!({"id":"host-mode","configRevision":2})
                                };
                                let handle=format!("body-{id}");bodies.insert(handle.clone(),Some(json!({"code":200,"data":data}).to_string().into_bytes()));
                                (json!({"status":200,"version":"HTTP/1.1","headers":[],"body":{"kind":"handle","handle":handle},"response":null,"session":false}),vec![])
                            }
                            "host.http.body_read"=>{let bytes=bodies.get_mut(params["handle"].as_str().unwrap()).unwrap().take();match bytes {Some(bytes)=>(json!({"eof":false,"trailers":null}),bytes),None=>(json!({"eof":true,"trailers":null}),vec![])}},
                            // 不允许 auth.get/save、OAuth 网络或私有状态写入，接入本身无需凭据明文
                            method=>panic!("Unexpected callback while reusing host account: {method}"),
                        };
                        send(&mut writer,Message::Result{id,result},payload).await;
                    }
                    Message::Result{result,..}=>{
                        let response:ManagementResponse=serde_json::from_value(result).unwrap();let data:Value=serde_json::from_slice(&frame.payload).unwrap();assert_eq!(response.status,200,"{}",data);
                        if method=="GET" {
                            if detach {assert_eq!(data["accounts"][0]["credentialSource"],"host");assert_eq!(data["accounts"][0]["auth"]["status"],"host_managed");assert_eq!(data["hostAccounts"][0]["id"],"native-account");}
                            else {assert_eq!(data["hostAccounts"][0]["label"],"本体账号");assert_eq!(data["accounts"],json!([]));}
                        }
                        break;
                    }
                    Message::Error{error,..}=>panic!("Host account management failed: {}",error.message),
                    message=>panic!("Unexpected frame: {message:?}"),
                }
            }
        }
        assert_eq!(updates,1);send(&mut writer,Message::Shutdown,vec![]).await;assert!(child.wait().await.unwrap().success());
    }).await.expect("Host account fixture timed out");
}

#[tokio::test]
async fn host_account_can_be_attached_without_login_or_copying_tokens() {
    run(false, false).await;
}
#[tokio::test]
async fn detaching_restores_native_execution_without_logging_out_host_account() {
    run(true, false).await;
}

#[tokio::test]
async fn group_and_individual_union_is_recomputed_by_backend_before_saving() {
    run(false, true).await;
}
