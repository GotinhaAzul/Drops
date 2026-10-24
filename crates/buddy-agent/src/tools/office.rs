//! Embedded, dependency-free OOXML helper. All paths are workspace confined.
use super::{process, string_arg, ToolContext, ToolOutcome};
use crate::paths;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(super) fn definition(edit: bool) -> Value {
    let mut properties = json!({
        "path":{"type":"string","description":"Saved .docx or .xlsx path inside the tool workspace. Use working_context.document.path when available."},
        "sheet":{"type":"string","description":"Exact XLSX sheet name. Omit on read to list sheets."}
    });
    if edit {
        properties["output_path"] = json!({"type":"string","description":"New copy path inside workspace, same extension. Must not exist. Original is never overwritten."});
        properties["expected_sha256"] = json!({"type":"string","description":"sha256 from a recent document_read, required to reject stale source content."});
        properties["replacements"] = json!({"type":"array","minItems":1,"maxItems":50,"description":"DOCX exact text replacements, each uniquely matched within a paragraph. No tabs/newlines, fields or tracked changes.","items":{"type":"object","properties":{"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["old_text","new_text"]}});
        properties["cells"] = json!({"type":"array","minItems":1,"maxItems":100,"description":"XLSX literal values in existing cells in the named sheet. Formula/error cells and entire array-formula ranges are read-only; formulas are recalculated by the app on opening.","items":{"type":"object","properties":{"cell":{"type":"string","description":"A1 reference"},"value":{"type":["string","number","boolean","null"]}},"required":["cell","value"]}});
    } else {
        properties["offset"] = json!({"type":"integer","minimum":1,"description":"1-based paragraph or stored-cell offset, default 1."});
        properties["limit"] = json!({"type":"integer","minimum":1,"maximum":100,"description":"Maximum paragraphs or stored cells, default 50."});
    }
    json!({"type":"function","function":{
        "name":if edit {"document_edit"}else{"document_read"},
        "description":if edit {"Create an edited copy of a saved DOCX/XLSX document after approval. Read it first for its fingerprint. Supports simple Word text replacements and existing Excel literal cells. Preserves other ZIP parts and never overwrites the source or existing output. No macros, encryption, signatures or web documents. Requires Python 3."} else {"Inspect saved local DOCX/XLSX content and return a SHA-256 fingerprint. Word paragraphs or Excel sheet/cell values, including stored formulas. Does not see unsaved app changes, run macros, or calculate formulas. UTF-8 JSON output. Requires Python 3."},
        "parameters":{"type":"object","properties":properties,"required":if edit {vec!["path","output_path","expected_sha256"]}else{vec!["path"]}}
    }})
}

pub(super) async fn execute(
    ctx: &ToolContext,
    args: &Value,
    edit: bool,
    cancel: &CancellationToken,
) -> ToolOutcome {
    let result = async {
        let requested = string_arg(args,"path").map_err(|e| anyhow::anyhow!(e.content))?;
        let path = paths::confine(&ctx.root,requested)?;
        let mut request = args.as_object().cloned().ok_or_else(||anyhow::anyhow!("arguments must be an object"))?;
        request.insert("path".into(),json!(path));
        request.insert("_root".into(),json!(ctx.root));
        request.insert("_edit".into(),json!(edit));
        if edit {
            let output = string_arg(args,"output_path").map_err(|e| anyhow::anyhow!(e.content))?;
            request.insert("output_path".into(),json!(paths::confine(&ctx.root,output)?));
        }
        let mut command = tokio::process::Command::new("python3");
        command.args(["-I","-c",include_str!("office.py")]).current_dir(&ctx.root)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        process::isolated(&mut command);
        let child = command.spawn().map_err(|_|anyhow::anyhow!("Python 3 is required for document tools"))?;
        let output = process::run(child,Some(serde_json::to_vec(&request)?),Duration::from_secs(15),cancel).await;
        if output.cancelled {anyhow::bail!("cancelled; if an edit copy was being published, inspect the output path before retrying");}
        if output.timed_out {anyhow::bail!("document operation timed out; check output before retrying");}
        let response: Value = serde_json::from_str(&output.stdout).map_err(|_|anyhow::anyhow!("document helper returned invalid output"))?;
        if let Some(error) = response.get("error").and_then(Value::as_str) {anyhow::bail!("{error}");}
        if !output.status.is_some_and(|status|status.success()) || output.error.is_some() {anyhow::bail!("document helper failed");}
        response.get("result").cloned().ok_or_else(||anyhow::anyhow!("missing document result"))
    }.await;
    match result {
        Ok(value) => ToolOutcome::ok(value.to_string()),
        Err(err) => ToolOutcome::error(format!("document: {err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn embedded_helper_reads_edits_and_verifies_a_copy() {
        let temp = crate::testutil::TempDir::new();
        let ctx = ToolContext {
            root: crate::paths::canonical_root(temp.path()).unwrap(),
            allow_shell: false,
            skills: vec![],
        };
        let source = ctx.root.join("note.docx");
        let generated = std::process::Command::new("python3").args(["-I","-c",r#"
import sys,zipfile
with zipfile.ZipFile(sys.argv[1],'w') as z:
 z.writestr('word/document.xml','<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello coworker</w:t></w:r></w:p></w:body></w:document>')
"#]).arg(&source).status().unwrap();
        assert!(generated.success());
        let cancel = CancellationToken::new();
        let read = execute(&ctx, &json!({"path":"note.docx"}), false, &cancel).await;
        assert!(!read.is_error, "{}", read.content);
        let value: Value = serde_json::from_str(&read.content).unwrap();
        let edit = execute(&ctx,&json!({"path":"note.docx","output_path":"copy.docx","expected_sha256":value["sha256"],"replacements":[{"old_text":"coworker","new_text":"friend"}]}),true,&cancel).await;
        assert!(!edit.is_error, "{}", edit.content);
        let verified = execute(&ctx, &json!({"path":"copy.docx"}), false, &cancel).await;
        assert!(verified.content.contains("Hello friend"));
        let original = execute(&ctx, &json!({"path":"note.docx"}), false, &cancel).await;
        assert!(original.content.contains("Hello coworker"));
        assert!(
            execute(&ctx, &json!({"path":"../outside.docx"}), false, &cancel)
                .await
                .is_error
        );
    }
}
