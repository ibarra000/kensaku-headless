//! Human-readable rendering. `--json` bypasses all of this.

use serde_json::Value;

pub fn json(value: &Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{value}"),
    }
}

pub fn scalar_or_json(value: &Value) {
    match value {
        Value::String(s) => println!("{s}"),
        Value::Number(_) | Value::Bool(_) | Value::Null => println!("{value}"),
        other => json(other),
    }
}

pub fn lines(value: &Value) {
    value
        .as_array()
        .into_iter()
        .flatten()
        .for_each(scalar_or_json);
}

pub fn join_paths(value: &Value) -> String {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// `path<TAB>score<TAB>snippet` per line (stable for scripts).
pub fn search_results(response: &Value) {
    for result in response["results"].as_array().into_iter().flatten() {
        println!(
            "{}\t{:.4}\t{}",
            result["path"].as_str().unwrap_or(""),
            result["score"].as_f64().unwrap_or(0.0),
            result["snippet"]
                .as_str()
                .unwrap_or("")
                .replace(['\n', '\t'], " ")
        );
    }
}

pub fn status_summary(status: &Value) -> String {
    let phase = status["phase"].as_str().unwrap_or("idle");
    let files = status["files_indexed"].as_u64().unwrap_or(0);
    let content = status["content_indexed"].as_u64().unwrap_or(0);
    let done = status["extract_done"].as_u64().unwrap_or(0);
    let total = status["extract_total"].as_u64().unwrap_or(0);
    let paused = if status["paused"].as_bool() == Some(true) {
        " (paused)"
    } else {
        ""
    };
    let progress = if total > 0 {
        format!(" extract {done}/{total}")
    } else {
        String::new()
    };
    format!("{phase}{paused}: {files} files, {content} with content{progress}")
}

pub fn status_line(status: &Value) {
    println!("{}", status_summary(status));
    if let Some(message) = status["current_status"].as_str().filter(|m| !m.is_empty()) {
        println!("{message}");
    }
}

pub fn doctor(report: &Value) {
    for check in report["checks"].as_array().into_iter().flatten() {
        let level = check["status"].as_str().unwrap_or("?");
        let label = check["label"].as_str().unwrap_or("");
        let detail = check["detail"].as_str().unwrap_or("");
        println!("[{level}] {label}: {detail}");
        if let Some(fix) = check["fix"].as_str().filter(|f| !f.is_empty()) {
            println!("        fix: {fix}");
        }
        if let Some(action) = check.pointer("/action/kind").and_then(Value::as_str) {
            println!("        run: kensaku doctor --fix {action}");
        }
    }
}

pub fn packs(status: &Value) {
    let Some(packs) = status.as_object() else {
        return json(status);
    };
    for (name, pack) in packs {
        let state = if pack["downloading"].as_bool() == Some(true) {
            "downloading".to_string()
        } else if let Some(err) = pack["error"].as_str().filter(|e| !e.is_empty()) {
            format!("error: {err}")
        } else if pack["ready"].as_bool() == Some(true) || pack["available"].as_bool() == Some(true)
        {
            "ready".to_string()
        } else {
            "not installed".to_string()
        };
        println!("{name:<9} {state}");
    }
}

pub fn methods() {
    for spec in kensaku_core::api::METHODS {
        let access = match spec.access {
            kensaku_core::api::Access::Read => "read ",
            kensaku_core::api::Access::FileRead => "file ",
            kensaku_core::api::Access::Write => "write",
        };
        println!("{:<22} {access}  {}", spec.name, spec.summary);
    }
}
