use std::fmt::Write;

use serde_json::Value;

use crate::commands::search::SearchOutput;
use crate::protocol::Response;

/// Format response as compact, token-optimized output for LLM consumption.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn format(response: &Response) -> String {
    if let Some(error) = &response.error {
        let mut out = format!("error: {} ({})", error.message, error.code);
        if let Some(advice) = &error.advice {
            let _ = write!(out, "\nadvice: {advice}");
        }
        return out;
    }

    let Some(data) = &response.data else {
        return String::new();
    };

    // Status response
    if data.get("daemon").is_some() {
        return format_status(data);
    }

    // Init response: has "files_indexed" key
    if data.get("files_indexed").is_some() {
        return format_init(data);
    }

    // File content: has "content" + "path" keys
    if data.get("content").is_some() && data.get("path").is_some() {
        return format_file_content(data);
    }

    // Directory symbols: has "dir": true
    if data.get("dir").and_then(Value::as_bool).unwrap_or(false) {
        return format_dir_symbols(data);
    }

    // Check response: has "diagnostics" key
    if let Some(diags) = data.get("diagnostics").and_then(|v| v.as_array()) {
        return format_check(data, diags);
    }

    // Edit replace: has "lines_before" key
    if data.get("lines_before").is_some() {
        return crate::commands::edit::format_replace(data);
    }

    // Hover response: has "hover_content" key
    if data.get("hover_content").is_some() {
        return format_hover(data);
    }

    // Format response: has "edits_applied" key
    if data.get("edits_applied").is_some() {
        return format_format(data);
    }

    // Rename response: has "files_changed" key
    if data.get("files_changed").is_some() {
        return format_rename(data);
    }

    // Fix response: has "fixes_applied" key
    if data.get("fixes_applied").is_some() {
        return format_fix(data);
    }

    // Server restart: {"restarted": lang, "server_name": name}
    if let Some(lang) = data.get("restarted").and_then(Value::as_str) {
        let server = data.get("server_name").and_then(Value::as_str).unwrap_or("?");
        return format!("restarted {lang} ({server})");
    }

    // Server clean: {"cleaned": true, ...}
    if data.get("cleaned").and_then(Value::as_bool).unwrap_or(false) {
        let bytes = data.get("bytes_freed").and_then(Value::as_u64).unwrap_or(0);
        if bytes == 0 {
            return "nothing to clean".to_string();
        }
        #[allow(clippy::cast_precision_loss)]
        let mb = bytes as f64 / 1_048_576.0;
        return format!("cleaned ~/.krait/servers/ ({mb:.1} MB freed)");
    }

    // Server install: {"installed": binary, "path": ...}
    if let Some(binary) = data.get("installed").and_then(Value::as_str) {
        let path = data.get("path").and_then(Value::as_str).unwrap_or("?");
        return format!("installed {binary} → {path}");
    }

    // Server status from daemon: {"servers": [...], "count": N}
    if let Some(servers) = data.get("servers").and_then(Value::as_array) {
        return format_daemon_server_status(servers);
    }

    // Edit insert: has "inserted_at" + "operation" keys
    if data.get("inserted_at").is_some() {
        let kind = data.get("operation").and_then(|v| v.as_str()).unwrap_or("after");
        return crate::commands::edit::format_insert(data, kind);
    }

    // Array results (symbol search, references, document symbols)
    if let Some(items) = data.as_array() {
        if items.is_empty() {
            return "no results".to_string();
        }

        let mut out = String::new();

        // Document symbols: {name, kind, line, children}
        if items.first().and_then(|i| i.get("name")).is_some()
            && items.first().and_then(|i| i.get("path")).is_none()
        {
            format_symbol_tree(items, &mut out, 0);
            return out.trim_end().to_string();
        }

        // Enriched references: has "containing_symbol"
        let is_enriched = items.iter().any(|i| i.get("containing_symbol").is_some());
        if is_enriched {
            format_enriched_refs(items, &mut out);
            return out.trim_end().to_string();
        }

        // Symbol search / references: {path, line, kind?, preview, body?}
        for item in items {
            if let Some(path) = item.get("path").and_then(Value::as_str) {
                let line = item.get("line").and_then(Value::as_u64).unwrap_or(0);
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
                let preview = item.get("preview").and_then(Value::as_str).unwrap_or("");
                let is_def = item
                    .get("is_definition")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let tag = if is_def { " [definition]" } else { "" };

                if kind.is_empty() {
                    let _ = writeln!(out, "{path}:{line} {preview}{tag}");
                } else {
                    let _ = writeln!(out, "{path}:{line} {kind} {preview}{tag}");
                }

                // Inline body when present (--include-body)
                if let Some(body) = item.get("body").and_then(Value::as_str) {
                    for (i, body_line) in body.lines().enumerate() {
                        #[allow(clippy::cast_possible_truncation)]
                        let num = line as usize + i;
                        let _ = writeln!(out, "  {num:>4}\t{body_line}");
                    }
                    let _ = writeln!(out, "---");
                }
            }
        }

        return out.trim_end().to_string();
    }

    // Generic: compact JSON on one line
    serde_json::to_string(data).unwrap_or_default()
}

fn format_init(data: &Value) -> String {
    let files = data
        .get("files_indexed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = data
        .get("files_cached")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let symbols = data
        .get("symbols_total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = data
        .get("files_total")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let elapsed = data
        .get("elapsed_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let time_str = if elapsed >= 1000 {
        format!(" in {}.{}s", elapsed / 1000, (elapsed % 1000) / 100)
    } else if elapsed > 0 {
        format!(" in {elapsed}ms")
    } else {
        String::new()
    };

    if cached > 0 {
        format!("indexed {files}/{total} files ({cached} cached), {symbols} symbols{time_str}")
    } else {
        format!("indexed {files} files, {symbols} symbols{time_str}")
    }
}

fn format_file_content(data: &Value) -> String {
    let path = data.get("path").and_then(Value::as_str).unwrap_or("?");
    let from = data.get("from").and_then(Value::as_u64).unwrap_or(0);
    let to = data.get("to").and_then(Value::as_u64).unwrap_or(0);
    let total = data.get("total").and_then(Value::as_u64);
    let truncated = data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content = data.get("content").and_then(Value::as_str).unwrap_or("");

    let mut header = String::new();

    // Symbol read: has "symbol" + "kind"
    if let Some(symbol) = data.get("symbol").and_then(Value::as_str) {
        let kind = data.get("kind").and_then(Value::as_str).unwrap_or("?");
        let _ = write!(header, "{kind} {symbol} in {path} ({from}-{to})");
    } else {
        // File read
        let _ = write!(header, "{path} ({from}-{to}");
        if let Some(t) = total {
            let _ = write!(header, "/{t}");
        }
        header.push(')');
    }

    if truncated {
        header.push_str(" [truncated]");
    }

    format!("{header}\n{}", content.trim_end())
}

fn format_status(data: &Value) -> String {
    let daemon = &data["daemon"];
    let pid = daemon.get("pid").and_then(Value::as_u64).unwrap_or(0);
    let uptime = daemon
        .get("uptime_secs")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = format!("daemon: pid={pid} uptime={}", format_duration(uptime));

    // Show config source (only if not auto-detected)
    if let Some(config) = data.get("config").and_then(|v| v.as_str()) {
        if config != "auto-detected" {
            let workspace_count = data
                .get("project")
                .and_then(|p| p.get("workspaces"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let _ = write!(out, "\nconfig: {config} ({workspace_count} workspaces)");
        }
    }

    if let Some(lsp) = data.get("lsp") {
        if !lsp.is_null() {
            format_lsp_status(lsp, data, &mut out);
        }
    }

    if let Some(project) = data.get("project") {
        let discovered = project
            .get("workspaces_discovered")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let attached = project
            .get("workspaces_attached")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if discovered > 0 {
            let _ = write!(out, "\nworkspaces: {discovered} discovered, {attached} attached");
        }

        if let Some(langs) = project.get("languages").and_then(|v| v.as_array()) {
            let names: Vec<&str> = langs.iter().filter_map(|v| v.as_str()).collect();
            if !names.is_empty() {
                let _ = write!(out, "\nproject: languages=[{}]", names.join(","));
            }
        }
    }

    // Index / watcher status
    if let Some(index) = data.get("index") {
        let watcher = index
            .get("watcher_active")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let dirty = index
            .get("dirty_files")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if watcher {
            let _ = write!(out, "\nindex: watcher active, {dirty} dirty files");
        } else {
            let _ = write!(out, "\nindex: watcher inactive (BLAKE3 fallback)");
        }
    }

    out
}

fn format_lsp_status(lsp: &Value, _data: &Value, out: &mut String) {
    let lsp_status = lsp.get("status").and_then(|v| v.as_str()).unwrap_or("?");
    let progress = lsp.get("progress").and_then(|v| v.as_str()).unwrap_or("");

    if let Some(servers) = lsp.get("servers").and_then(|v| v.as_array()) {
        let sessions = lsp
            .get("sessions")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let status_tag = if lsp_status != "ready" && !progress.is_empty() {
            format!(" [{lsp_status} {progress}]")
        } else {
            String::new()
        };
        let _ = write!(out, "\nlsp: {sessions} servers{status_tag}");

        for s in servers {
            let lang = s.get("language").and_then(|v| v.as_str()).unwrap_or("?");
            let server = s.get("server").and_then(|v| v.as_str()).unwrap_or("?");
            let s_status = s.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let attached = s.get("attached_folders").and_then(Value::as_u64).unwrap_or(0);
            let total = s.get("total_folders").and_then(Value::as_u64).unwrap_or(0);
            let state_tag = if s_status == "ready" {
                String::new()
            } else {
                format!(" [{s_status}]")
            };
            let folders = format!("{attached}/{total} folders");
            let _ = write!(out, "\n  {lang} ({server}) — {folders}{state_tag}");
        }
    } else if lsp_status == "pending" && !progress.is_empty() {
        let _ = write!(out, "\nlsp: pending ({progress})");
    } else {
        let lang = lsp.get("language").and_then(|v| v.as_str()).unwrap_or("?");
        let server = lsp.get("server").and_then(|v| v.as_str()).unwrap_or("?");
        let _ = write!(out, "\nlsp: {lang} {lsp_status} ({server})");
    }
}

fn format_dir_symbols(data: &Value) -> String {
    let files = match data.get("files").and_then(Value::as_array) {
        Some(f) if !f.is_empty() => f,
        _ => return "no results".to_string(),
    };

    let mut out = String::new();
    for (i, entry) in files.iter().enumerate() {
        let file = entry.get("file").and_then(Value::as_str).unwrap_or("?");
        let _ = writeln!(out, "{file}");
        if let Some(symbols) = entry.get("symbols").and_then(Value::as_array) {
            format_symbol_tree(symbols, &mut out, 1);
        }
        if i + 1 < files.len() {
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Format references enriched with `--with-symbol`.
///
/// Each reference is printed as:
///   `path:line  [in containingFn (kind:N)]  preview`
/// Definition sites are printed as:
///   `path:line  [definition]  preview`
fn format_enriched_refs(items: &[Value], out: &mut String) {
    for item in items {
        let path = item.get("path").and_then(Value::as_str).unwrap_or("?");
        let line = item.get("line").and_then(Value::as_u64).unwrap_or(0);
        let preview = item.get("preview").and_then(Value::as_str).unwrap_or("").trim();
        let is_def = item.get("is_definition").and_then(Value::as_bool).unwrap_or(false);

        if is_def {
            let _ = writeln!(out, "{path}:{line}  [definition]  {preview}");
            continue;
        }

        let tag = if let Some(cs) = item.get("containing_symbol") {
            let sym_name = cs.get("name").and_then(Value::as_str).unwrap_or("?");
            let sym_kind = cs.get("kind").and_then(Value::as_str).unwrap_or("?");
            let sym_line = cs.get("line").and_then(Value::as_u64).unwrap_or(0);
            format!("  [in {sym_name} ({sym_kind}:{sym_line})]")
        } else {
            String::new()
        };

        let _ = writeln!(out, "{path}:{line}{tag}  {preview}");
    }
}

fn format_symbol_tree(items: &[Value], out: &mut String, indent: usize) {
    for item in items {
        let name = item.get("name").and_then(Value::as_str).unwrap_or("?");
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or("?");
        let prefix = "  ".repeat(indent);
        let _ = writeln!(out, "{prefix}{kind} {name}");
        if let Some(children) = item.get("children").and_then(Value::as_array) {
            format_symbol_tree(children, out, indent + 1);
        }
    }
}

fn format_check(data: &Value, diags: &[Value]) -> String {
    if diags.is_empty() {
        return "No diagnostics".to_string();
    }

    let mut out = String::new();
    for d in diags {
        let sev = d.get("severity").and_then(Value::as_str).unwrap_or("?");
        let path = d.get("path").and_then(Value::as_str).unwrap_or("?");
        let line = d.get("line").and_then(Value::as_u64).unwrap_or(0);
        let col = d.get("col").and_then(Value::as_u64).unwrap_or(0);
        let code = d
            .get("code")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("");
        let msg = d.get("message").and_then(Value::as_str).unwrap_or("");

        if code.is_empty() {
            let _ = writeln!(out, "{sev:<5} {path}:{line}:{col} {msg}");
        } else {
            let _ = writeln!(out, "{sev:<5} {path}:{line}:{col} {code} {msg}");
        }
    }

    let total = data.get("total").and_then(Value::as_u64).unwrap_or(0);
    let errors = data.get("errors").and_then(Value::as_u64).unwrap_or(0);
    let warnings = data.get("warnings").and_then(Value::as_u64).unwrap_or(0);

    let mut summary = format!("{total} diagnostic");
    if total != 1 {
        summary.push('s');
    }

    let mut parts: Vec<String> = vec![];
    if errors > 0 {
        parts.push(format!("{errors} error{}", if errors == 1 { "" } else { "s" }));
    }
    if warnings > 0 {
        parts.push(format!("{warnings} warning{}", if warnings == 1 { "" } else { "s" }));
    }
    if !parts.is_empty() {
        let joined = parts.join(", ");
        summary.push_str(" (");
        summary.push_str(&joined);
        summary.push(')');
    }

    out.push_str(&summary);
    out
}

fn format_hover(data: &Value) -> String {
    let content = data.get("hover_content").and_then(Value::as_str).unwrap_or("").trim();
    let path = data.get("path").and_then(Value::as_str).unwrap_or("?");
    let line = data.get("line").and_then(Value::as_u64).unwrap_or(0);

    if content.is_empty() {
        return format!("No hover information available ({path}:{line})");
    }

    format!("{content}\n{path}:{line}")
}

fn format_format(data: &Value) -> String {
    let path = data.get("path").and_then(Value::as_str).unwrap_or("?");
    let n = data.get("edits_applied").and_then(Value::as_u64).unwrap_or(0);
    if n == 0 {
        format!("No changes needed ({path})")
    } else {
        format!("Formatted {path} ({n} edits)")
    }
}

fn format_rename(data: &Value) -> String {
    let files = data.get("files_changed").and_then(Value::as_u64).unwrap_or(0);
    let refs = data.get("refs_changed").and_then(Value::as_u64).unwrap_or(0);
    if files == 0 {
        "No references renamed".to_string()
    } else {
        format!("Renamed {refs} refs across {files} files")
    }
}

fn format_fix(data: &Value) -> String {
    let n = data.get("fixes_applied").and_then(Value::as_u64).unwrap_or(0);
    if n == 0 {
        return "No fixes available".to_string();
    }

    let files: Vec<&str> = data
        .get("files")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let file_list = files.join(", ");
    format!("Applied {n} fix(es) in {file_list}")
}

/// Format search results as compact output.
#[must_use]
pub fn format_search(output: &SearchOutput, with_context: bool, files_only: bool) -> String {
    let mut out = String::new();

    if files_only {
        let mut seen = std::collections::BTreeSet::new();
        for m in &output.matches {
            seen.insert(m.path.as_str());
        }
        for path in &seen {
            let _ = writeln!(out, "{path}");
        }
        let n = seen.len();
        let _ = write!(out, "{n} {}", if n == 1 { "file" } else { "files" });
        return out;
    }

    if with_context {
        format_search_with_context(output, &mut out);
    } else {
        format_search_flat(output, &mut out);
    }

    out
}

fn format_search_flat(output: &SearchOutput, out: &mut String) {
    // Compute max width of "path:line:col" prefix for alignment
    let max_loc_len = output
        .matches
        .iter()
        .map(|m| format!("{}:{}:{}", m.path, m.line, m.column).len())
        .max()
        .unwrap_or(0);

    for m in &output.matches {
        let loc = format!("{}:{}:{}", m.path, m.line, m.column);
        let _ = writeln!(
            out,
            "{loc:<width$}  {preview}",
            width = max_loc_len,
            preview = m.preview.trim()
        );
    }

    let n = output.total_matches;
    let f = output.files_with_matches;
    let trunc = if output.truncated { " [truncated]" } else { "" };
    let _ = write!(
        out,
        "{n} {} in {f} {}{}",
        if n == 1 { "match" } else { "matches" },
        if f == 1 { "file" } else { "files" },
        trunc,
    );
}

fn format_search_with_context(output: &SearchOutput, out: &mut String) {
    // Group matches by file, preserving order
    let mut current_file: Option<&str> = None;

    for m in &output.matches {
        if current_file != Some(m.path.as_str()) {
            if current_file.is_some() {
                out.push_str("──\n");
            }
            let _ = writeln!(out, "{}", m.path);
            current_file = Some(m.path.as_str());
        }

        // Compute line number width for this block
        let max_line = m.line as usize + m.context_after.len();
        let width = max_line.to_string().len();

        let start_line = m.line as usize - m.context_before.len();
        for (i, ctx) in m.context_before.iter().enumerate() {
            let lno = start_line + i;
            let _ = writeln!(out, "  {lno:>width$}  {ctx}");
        }
        let _ = writeln!(out, "> {:>width$}  {}", m.line, m.preview.trim());
        for (i, ctx) in m.context_after.iter().enumerate() {
            let lno = m.line as usize + 1 + i;
            let _ = writeln!(out, "  {lno:>width$}  {ctx}");
        }
    }

    if current_file.is_some() {
        out.push_str("──\n");
    }

    let n = output.total_matches;
    let f = output.files_with_matches;
    let trunc = if output.truncated { " [truncated]" } else { "" };
    let _ = write!(
        out,
        "{n} {} in {f} {}{}",
        if n == 1 { "match" } else { "matches" },
        if f == 1 { "file" } else { "files" },
        trunc,
    );
}

fn format_daemon_server_status(servers: &[Value]) -> String {
    if servers.is_empty() {
        return "no servers running".to_string();
    }
    let mut out = String::new();
    for s in servers {
        let lang = s.get("language").and_then(Value::as_str).unwrap_or("?");
        let server = s.get("server").and_then(Value::as_str).unwrap_or("?");
        let status = s.get("status").and_then(Value::as_str).unwrap_or("?");
        let attached = s.get("attached_folders").and_then(Value::as_u64).unwrap_or(0);
        let total = s.get("total_folders").and_then(Value::as_u64).unwrap_or(0);
        let uptime = s.get("uptime_secs").and_then(Value::as_u64).unwrap_or(0);
        let uptime_str = if uptime > 0 {
            format!(" uptime={}", format_duration(uptime))
        } else {
            String::new()
        };
        let state_tag = if status == "ready" {
            String::new()
        } else {
            format!(" [{status}]")
        };
        let _ = writeln!(
            out,
            "{lang:<12}  {server:<24}  {attached}/{total} folders{state_tag}{uptime_str}"
        );
    }
    out.trim_end().to_string()
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m}m")
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn compact_status_output() {
        let resp = Response::ok(json!({"daemon": {"pid": 12345, "uptime_secs": 300}}));
        let out = format(&resp);
        assert_eq!(out, "daemon: pid=12345 uptime=5m");
    }

    #[test]
    fn compact_error_output() {
        let resp = Response::err_with_advice("lsp_not_found", "LSP not detected", "Install it");
        let out = format(&resp);
        assert!(out.contains("error: LSP not detected"));
        assert!(out.contains("advice: Install it"));
    }

    #[test]
    fn compact_symbol_results() {
        let resp = Response::ok(json!([
            {"path": "src/lib.rs", "line": 5, "kind": "function", "preview": "fn greet(name: &str) -> String"},
            {"path": "src/lib.rs", "line": 15, "kind": "struct", "preview": "struct Config"}
        ]));
        let out = format(&resp);
        assert!(out.contains("src/lib.rs:5 function fn greet"));
        assert!(out.contains("src/lib.rs:15 struct struct Config"));
    }

    #[test]
    fn compact_reference_results() {
        let resp = Response::ok(json!([
            {"path": "src/lib.rs", "line": 5, "preview": "pub fn greet()", "is_definition": true},
            {"path": "src/main.rs", "line": 8, "preview": "let msg = greet(\"world\");", "is_definition": false}
        ]));
        let out = format(&resp);
        assert!(out.contains("[definition]"));
        assert!(out.contains("src/main.rs:8"));
    }

    #[test]
    fn compact_empty_results() {
        let resp = Response::ok(json!([]));
        let out = format(&resp);
        assert_eq!(out, "no results");
    }

    #[test]
    fn compact_file_content_output() {
        let resp = Response::ok(json!({
            "path": "src/main.rs",
            "content": "   1\tfn main() {\n   2\t    println!(\"hello\");\n   3\t}\n",
            "from": 1,
            "to": 3,
            "total": 3,
            "truncated": false,
        }));
        let out = format(&resp);
        assert!(out.starts_with("src/main.rs (1-3/3)"));
        assert!(out.contains("fn main()"));
    }

    #[test]
    fn compact_file_content_truncated() {
        let resp = Response::ok(json!({
            "path": "big.rs",
            "content": "   1\tline1\n",
            "from": 1,
            "to": 200,
            "total": 500,
            "truncated": true,
        }));
        let out = format(&resp);
        assert!(out.contains("[truncated]"));
    }

    #[test]
    fn compact_symbol_content_output() {
        let resp = Response::ok(json!({
            "path": "src/lib.rs",
            "symbol": "Config",
            "kind": "struct",
            "content": "   5\tpub struct Config {\n   6\t    name: String,\n   7\t}\n",
            "from": 5,
            "to": 7,
            "truncated": false,
        }));
        let out = format(&resp);
        assert!(out.starts_with("struct Config in src/lib.rs (5-7)"));
        assert!(out.contains("pub struct Config"));
    }

    #[test]
    fn compact_check_with_diagnostics() {
        let resp = Response::ok(json!({
            "diagnostics": [
                {"severity": "error", "path": "src/lib.rs", "line": 42, "col": 10, "code": "E0308", "message": "mismatched types"},
                {"severity": "warn", "path": "src/main.rs", "line": 3, "col": 5, "code": "", "message": "unused import"}
            ],
            "total": 2,
            "errors": 1,
            "warnings": 1,
        }));
        let out = format(&resp);
        assert!(out.contains("error src/lib.rs:42:10 E0308 mismatched types"));
        assert!(out.contains("warn  src/main.rs:3:5 unused import"));
        assert!(out.contains("2 diagnostics"));
        assert!(out.contains("1 error"));
        assert!(out.contains("1 warning"));
    }

    #[test]
    fn compact_check_no_diagnostics() {
        let resp = Response::ok(json!({
            "diagnostics": [],
            "total": 0,
            "errors": 0,
            "warnings": 0,
        }));
        let out = format(&resp);
        assert_eq!(out, "No diagnostics");
    }

    #[test]
    fn compact_duration_formatting() {
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(300), "5m");
        assert_eq!(format_duration(3600), "1h");
        assert_eq!(format_duration(3900), "1h5m");
    }
}
