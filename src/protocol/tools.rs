//! 客户端工具目录、OfficeJS 信封和隔离的有界原生历史回放，不执行工具代码

use super::{content, error, fingerprint, is_tool, structured, text};
use gateway_plugin_sdk::PluginFault;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};

pub type SharedReplay = Arc<Mutex<ReplayCache>>;

#[derive(Default)]
pub struct ReplayCache {
    entries: VecDeque<Entry>,
    bytes: usize,
}
struct Entry {
    scope: String,
    id: String,
    signature: String,
    native: Value,
    bytes: usize,
}

impl ReplayCache {
    pub fn put(&mut self, scope: &str, native: Value, client: &Value) {
        let id = text(&client["call_id"]);
        let bytes = serde_json::to_vec(&native).map_or(usize::MAX, |v| v.len());
        if id.is_empty() || bytes > 1024 * 1024 {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|e| e.scope == scope && e.id == id)
        {
            self.bytes -= self.entries.remove(index).unwrap().bytes;
        }
        self.entries.push_back(Entry {
            scope: scope.into(),
            id: id.into(),
            signature: call_signature(client),
            native,
            bytes,
        });
        self.bytes += bytes;
        while self.entries.len() > 1024 || self.bytes > 16 * 1024 * 1024 {
            if let Some(entry) = self.entries.pop_front() {
                self.bytes -= entry.bytes;
            }
        }
    }
    pub fn get(&mut self, scope: &str, id: &str, call: Option<&Value>) -> Option<Value> {
        let signature = call.map(call_signature);
        if signature.as_ref().is_some_and(String::is_empty) {
            return None;
        }
        let index = self.entries.iter().position(|e| {
            e.scope == scope && e.id == id && signature.as_ref().is_none_or(|s| s == &e.signature)
        })?;
        let entry = self.entries.remove(index)?;
        let value = entry.native.clone();
        self.entries.push_back(entry);
        Some(value)
    }
}

fn call_signature(call: &Value) -> String {
    if text(&call["call_id"]).trim().is_empty() || text(&call["name"]).trim().is_empty() {
        return String::new();
    }
    let mut canonical = json!({"type":call["type"],"call_id":call["call_id"],"name":call["name"],"namespace":text(&call["namespace"])});
    match text(&call["type"]) {
        "function_call" => match arguments(&call["arguments"]) {
            Ok(args) => canonical["arguments"] = args,
            Err(_) => return String::new(),
        },
        "custom_tool_call" if call["input"].is_string() => {
            canonical["input"] = call["input"].clone()
        }
        _ => return String::new(),
    }
    fingerprint(&canonical)
}

#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub namespace: String,
    pub kind: String,
    pub entry: Value,
    definition: Value,
}

#[derive(Default)]
pub struct Tools {
    pub catalog: BTreeMap<String, Tool>,
    pub unsupported: BTreeSet<String>,
}

impl Tools {
    pub fn merge(&mut self, raw: &Value) -> Result<(), PluginFault> {
        let mut batch = Self::default();
        batch.collect(raw, "", 0)?;
        for (name, tool) in batch.catalog {
            if self
                .catalog
                .get(&name)
                .is_some_and(|old| old.name != tool.name || old.namespace != tool.namespace)
            {
                return Err(error("Ambiguous BPS client tool namespace."));
            }
            self.catalog.insert(name, tool);
        }
        self.unsupported.extend(batch.unsupported);
        Ok(())
    }

    fn collect(&mut self, raw: &Value, namespace: &str, depth: usize) -> Result<(), PluginFault> {
        if raw.is_null() {
            return Ok(());
        }
        if depth > 16 {
            return Err(error("Client tool namespaces are nested too deeply."));
        }
        for item in raw
            .as_array()
            .ok_or_else(|| error("Client tools must be an array."))?
        {
            let kind = text(&item["type"]);
            let name = text(&item["name"]);
            if kind == "namespace" {
                if name.is_empty() {
                    return Err(error("Client tool namespace requires a name."));
                }
                let nested = if namespace.is_empty() {
                    name.into()
                } else {
                    format!("{namespace}.{name}")
                };
                self.collect(&item["tools"], &nested, depth + 1)?;
                continue;
            }
            if matches!(
                kind,
                "web_search"
                    | "web_search_preview"
                    | "web_search_preview_2025_03_11"
                    | "web_search_2025_08_26"
                    | "tool_search"
                    | "image_generation"
                    | "file_search"
                    | "code_interpreter"
                    | "computer"
                    | "computer_use_preview"
                    | "mcp"
            ) {
                self.unsupported.insert(kind.into());
                continue;
            }
            if !matches!(kind, "function" | "custom")
                || name.is_empty()
                || name.len() > 512
                || name.chars().any(char::is_control)
            {
                return Err(error("BPS requires named client function or custom tools."));
            }
            let key = if namespace.is_empty() {
                name.into()
            } else {
                format!("{namespace}.{name}")
            };
            let params = item
                .get("parameters")
                .or_else(|| item.get("inputSchema"))
                .or_else(|| item.get("input_schema"))
                .cloned()
                .unwrap_or(Value::Null);
            let mut entry = json!({"type":kind,"name":key});
            for field in ["description", "format"] {
                if let Some(value) = item.get(field) {
                    entry[field] = value.clone();
                }
            }
            if kind == "function" {
                entry["parameters"] = params.clone();
            }
            let definition = json!({"type":kind,"parameters":params,"format":item["format"],"strict":item["strict"]});
            if let Some(old) = self.catalog.get(&key) {
                if old.definition != definition || old.name != name || old.namespace != namespace {
                    return Err(error("Conflicting duplicate BPS client tool."));
                }
                continue;
            }
            self.catalog.insert(
                key,
                Tool {
                    name: name.into(),
                    namespace: namespace.into(),
                    kind: kind.into(),
                    entry,
                    definition,
                },
            );
            if self.catalog.len() > 1024 {
                return Err(error("BPS client catalog exceeds 1024 tools."));
            }
        }
        Ok(())
    }

    pub fn history(
        &self,
        input: &[Value],
        scope: &str,
        replay: &SharedReplay,
    ) -> Result<Vec<Value>, PluginFault> {
        let mut output = vec![];
        let mut seen = BTreeSet::new();
        let mut trigger = None;
        let mut cache = replay
            .lock()
            .map_err(|_| error("BPS replay cache is unavailable."))?;
        for original in input {
            let mut item = original.clone();
            let object = item
                .as_object_mut()
                .ok_or_else(|| error("BPS history items must be objects."))?;
            object.remove("internal_chat_message_metadata_passthrough");
            match text(&item["type"]) {
                "additional_tools" | "configuration_update" => continue,
                "item_reference" => {
                    return Err(error(
                        "BPS requires full history instead of item_reference.",
                    ));
                }
                "compaction_trigger" => {
                    trigger = Some(item);
                    continue;
                }
                "reasoning" => {
                    let encrypted = text(&item["encrypted_content"]);
                    if !encrypted.is_empty() {
                        output.push(
                            json!({"type":"reasoning","summary":[],"encrypted_content":encrypted}),
                        );
                    }
                    continue;
                }
                "function_call" | "custom_tool_call" => {
                    let id = text(&item["call_id"]).to_owned();
                    item = match cache.get(scope, &id, Some(&item)) {
                        Some(native) => native,
                        None => {
                            let native = rebuild(&item)?;
                            cache.put(scope, native.clone(), &item);
                            native
                        }
                    };
                    seen.insert(id);
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let id = text(&item["call_id"]).to_owned();
                    if !seen.contains(&id) {
                        output.push(cache.get(scope, &id, None).ok_or_else(|| {
                            error("Original BPS tool call is unavailable. Send complete history.")
                        })?);
                        seen.insert(id.clone());
                    }
                    item["type"] = json!("function_call_output");
                    content::validate(&item["output"], false)?;
                    let old = item["id"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("fc_{id}"));
                    item["id"] = json!(if old.starts_with("fc_") && old.len() <= 64 {
                        old
                    } else {
                        format!("fc_{}", fingerprint(&json!(old)))
                    });
                }
                _ => {}
            }
            content::validate(&item["content"], item["type"] == "agent_message")?;
            output.push(item);
        }
        if let Some(trigger) = trigger {
            output.push(trigger);
        }
        Ok(output)
    }

    pub fn translate(&self, native: &Value) -> Result<(Value, Value), PluginFault> {
        let name = text(&native["name"]);
        let (key, envelope, marked) = if matches!(name, "run_officejs" | "functions.run_officejs") {
            let outer = arguments(&native["arguments"])?;
            if let Some(key) = text(&outer["summary"]).strip_prefix("codex2api.custom/") {
                if key.is_empty()
                    || key.contains(['/', '\\'])
                    || key.chars().any(|c| c.is_whitespace() || c.is_control())
                    || outer["code"]
                        .as_str()
                        .is_none_or(|code| code.len() > 1024 * 1024)
                {
                    return Err(error(
                        "Invalid BPS raw custom-tool transport marker or input.",
                    ));
                }
                (key.to_owned(), json!({"input":outer["code"]}), true)
            } else {
                let envelope = unwrap_envelope(&outer["code"], &self.catalog)?;
                let key = alias(&envelope, "name", "tool")?
                    .as_str()
                    .ok_or_else(|| error("BPS tool envelope requires a name."))?
                    .to_owned();
                (key, envelope, false)
            }
        } else if matches!(name, "update_plan" | "functions.update_plan") {
            return self.plan(native);
        } else {
            let key = if self.catalog.contains_key(name) {
                name
            } else {
                name.strip_prefix("functions.").unwrap_or(name)
            };
            let tool = self.catalog.get(key).ok_or_else(|| {
                error("BPS returned an undeclared native tool. No tool was executed.")
            })?;
            if text(&native["type"]) != format!("{}_call", tool.kind) {
                return Err(error(
                    "BPS direct tool kind does not match its declaration.",
                ));
            }
            (
                key.into(),
                json!({"arguments":native["arguments"],"input":native["input"]}),
                false,
            )
        };
        let tool = self
            .catalog
            .get(&key)
            .ok_or_else(|| error("BPS returned a tool outside the client catalog."))?;
        let id = text(&native["call_id"]);
        if id.is_empty() {
            return Err(error("BPS tool call is missing call_id."));
        }
        let item_id = native["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("fc_{}", fingerprint(&json!(id))));
        let mut result = json!({"type":"function_call","id":item_id,"call_id":id,"name":tool.name,"status":"completed"});
        if !tool.namespace.is_empty() {
            result["namespace"] = json!(tool.namespace);
        }
        if tool.kind == "custom" {
            if envelope.get("arguments").is_some_and(|v| !v.is_null()) {
                return Err(error("Custom tools require raw input, not arguments."));
            }
            let input = alias(&envelope, "input", "args")?;
            if !input.is_string() {
                return Err(error("BPS custom tool input must be text."));
            }
            result["type"] = json!("custom_tool_call");
            result["id"] = json!(format!("ctc_{}", fingerprint(&json!(id))));
            result["input"] = input;
        } else {
            if marked {
                return Err(error("Raw BPS transport requires a custom tool."));
            }
            result["arguments"] =
                json!(arguments(&alias(&envelope, "arguments", "args")?)?.to_string());
        }
        let replay = if matches!(name, "run_officejs" | "functions.run_officejs") {
            native.clone()
        } else {
            rebuild(&result)?
        };
        Ok((result, replay))
    }

    fn plan(&self, native: &Value) -> Result<(Value, Value), PluginFault> {
        if native["type"] != "function_call" {
            return Err(error("BPS native update_plan must be a function call."));
        }
        let candidates: Vec<_> = ["update_plan", "functions.update_plan"]
            .iter()
            .filter_map(|key| self.catalog.get(*key))
            .collect();
        if candidates.len() != 1 || candidates[0].kind != "function" {
            return Err(error(
                "BPS update_plan requires one unambiguous client function declaration.",
            ));
        }
        let tool = candidates[0];
        let schema = &tool.entry["parameters"];
        if schema["type"] != "object" || schema["properties"]["plan"].is_null() {
            return Err(error("BPS update_plan requires an explicit plan schema."));
        }
        let args = arguments(&native["arguments"])?;
        let mut plan = vec![];
        for step in args["plan"]
            .as_array()
            .ok_or_else(|| error("BPS update_plan requires a plan array."))?
        {
            let description = text_alias(step, &["step", "description", "title"])?;
            if description.trim().is_empty() {
                return Err(error("BPS plan step is missing its description."));
            }
            let status = text(&step["status"])
                .trim()
                .to_ascii_lowercase()
                .replace(['-', ' '], "_");
            let status = match status.as_str() {
                "pending" | "not_started" | "todo" | "planned" | "queued" | "blocked" => "pending",
                "in_progress" | "active" | "started" | "doing" | "current" => "in_progress",
                "completed" | "complete" | "done" | "finished" => "completed",
                _ => return Err(error("Unsupported BPS native plan status.")),
            };
            plan.push(json!({"step":description,"status":status}));
        }
        let mut args_out = json!({"plan":plan});
        if args.get("explanation").is_some() || args.get("summary").is_some() {
            args_out["explanation"] = json!(text_alias(&args, &["explanation", "summary"])?);
        }
        if !structured::compile(schema)?.is_valid(&args_out) {
            return Err(error("BPS plan does not satisfy the client schema."));
        }
        if text(&native["call_id"]).is_empty() {
            return Err(error("BPS plan call is missing call_id."));
        }
        let mut result = native.clone();
        result["status"] = json!("completed");
        if text(&result["id"]).is_empty() {
            result["id"] = json!(format!("fc_{}", fingerprint(&native["call_id"])));
        }
        result["name"] = json!(tool.name);
        result["arguments"] = json!(args_out.to_string());
        if !tool.namespace.is_empty() {
            result["namespace"] = json!(tool.namespace);
        }
        Ok((result, native.clone()))
    }
}

fn text_alias(value: &Value, keys: &[&str]) -> Result<String, PluginFault> {
    let mut result: Option<&str> = None;
    for key in keys {
        if let Some(value) = value.get(*key) {
            let value = value
                .as_str()
                .ok_or_else(|| error("BPS plan text must be a string."))?;
            if result.is_some_and(|old| old != value) {
                return Err(error("Conflicting BPS plan text fields."));
            }
            result = Some(value);
        }
    }
    Ok(result.unwrap_or("").into())
}

fn arguments(value: &Value) -> Result<Value, PluginFault> {
    let value = if let Some(raw) = value.as_str() {
        serde_json::from_str(raw)
            .map_err(|_| error("BPS function arguments must be valid JSON."))?
    } else {
        value.clone()
    };
    if !value.is_object() {
        return Err(error("BPS function arguments must be a JSON object."));
    }
    Ok(value)
}

fn alias(value: &Value, key: &str, other: &str) -> Result<Value, PluginFault> {
    match (value.get(key), value.get(other)) {
        (Some(a), Some(b)) if a != b => Err(error("Conflicting BPS tool envelope fields.")),
        (Some(a), _) | (_, Some(a)) => Ok(a.clone()),
        _ => Ok(Value::Null),
    }
}

fn unwrap_envelope(value: &Value, catalog: &BTreeMap<String, Tool>) -> Result<Value, PluginFault> {
    let mut current = value.clone();
    for _ in 0..3 {
        let object = decode_envelope(&current)
            .or_else(|| recover(&current, catalog))
            .ok_or_else(|| error("Invalid BPS client-tool envelope. No code was executed."))?;
        let name = alias(&object, "name", "tool")?;
        if !matches!(text(&name), "run_officejs" | "functions.run_officejs") {
            return Ok(object);
        }
        current = arguments(&alias(&object, "arguments", "args")?)?["code"].clone();
    }
    Err(error("BPS tool envelope has too many nested wrappers."))
}

fn decode_envelope(value: &Value) -> Option<Value> {
    let mut value = value.clone();
    for _ in 0..4 {
        if value.is_object() {
            return Some(value);
        }
        let raw = value.as_str()?.trim();
        if raw.len() > 1024 * 1024 {
            return None;
        }
        if let Ok(next) = serde_json::from_str(raw) {
            value = next;
            continue;
        }
        let repaired = repair_json_strings(raw);
        if repaired != raw
            && let Ok(next) = serde_json::from_str(&repaired)
        {
            value = next;
            continue;
        }
        if let Some(start) = raw.find("```")
            && prose(&raw[..start])
            && raw.ends_with("```")
        {
            let fenced = &raw[start + 3..raw.len() - 3];
            let (lang, body) = fenced.split_once('\n')?;
            if lang.trim().is_empty() || lang.trim().eq_ignore_ascii_case("json") {
                value = json!(body.trim());
                continue;
            }
        }
        if let Some(start) = raw.find('{').filter(|i| *i > 0 && prose(&raw[..*i])) {
            value = json!(&raw[start..]);
            continue;
        }
        return None;
    }
    None
}

fn prose(value: &str) -> bool {
    value.len() <= 512 && !value.contains(['{', '}', '[', ']', '(', ')', ';', '=', '`', '"'])
}

fn recover(value: &Value, catalog: &BTreeMap<String, Tool>) -> Option<Value> {
    let raw = value.as_str()?;
    if raw.len() > 1024 * 1024 {
        return None;
    }
    let mut found = None;
    let mut offset = 0;
    while let Some(start) = raw[offset..].find('{').map(|i| offset + i) {
        let mut stream = serde_json::Deserializer::from_str(&raw[start..]).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next() {
            let end = stream.byte_offset();
            if let Ok(name) = alias(&value, "name", "tool")
                && catalog.contains_key(text(&name))
            {
                if found.is_some() {
                    return None;
                }
                found = Some(value);
            }
            offset = start + end.max(1);
        } else {
            offset = start + 1;
        }
        if offset >= raw.len() {
            break;
        }
    }
    found
}

fn repair_json_strings(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let mut quoted = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            quoted = !quoted;
        }
        if quoted && matches!(c, '\n' | '\r' | '\t') {
            out.push_str(match c {
                '\n' => "\\n",
                '\r' => "\\r",
                _ => "\\t",
            });
            i += 1;
            continue;
        }
        if quoted && c == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            let valid = "\"\\/bfnrt".contains(next)
                || (next == 'u'
                    && i + 5 < chars.len()
                    && chars[i + 2..i + 6].iter().all(char::is_ascii_hexdigit));
            if valid {
                out.push(c);
                out.push(next);
                i += 2;
                continue;
            }
            out.push('\\');
        }
        out.push(c);
        i += 1;
    }
    out
}

fn rebuild(item: &Value) -> Result<Value, PluginFault> {
    if !is_tool(item) || text(&item["call_id"]).is_empty() || text(&item["name"]).is_empty() {
        return Err(error(
            "BPS tool history requires complete call_id, name and arguments.",
        ));
    }
    let name = if text(&item["namespace"]).is_empty() {
        text(&item["name"]).into()
    } else {
        format!("{}.{}", text(&item["namespace"]), text(&item["name"]))
    };
    let mut envelope = json!({"name":name});
    if item["type"] == "function_call" {
        envelope["arguments"] = arguments(&item["arguments"])?;
    } else if item["input"].is_string() {
        envelope["input"] = item["input"].clone();
    } else {
        return Err(error("Custom tool history requires input text."));
    }
    let arguments = json!({"code":envelope.to_string(),"summary":"Replay a previously requested client tool","extended_summary":"Consume its recorded result without repeating it.","destructive":false,"references":[]});
    let id = text(&item["id"]);
    let id = if id.starts_with("fc_") && id.len() <= 64 {
        id.into()
    } else {
        format!("fc_{}", fingerprint(&item["call_id"]))
    };
    Ok(
        json!({"type":"function_call","id":id,"call_id":item["call_id"],"name":"run_officejs","arguments":arguments.to_string(),"status":"completed"}),
    )
}
