use anyhow::{Context as _, Result};
use jupyter_protocol::JupyterMessageContent;
use nbformat::{Notebook, v4};
use serde_json::{Value, json};

pub(super) fn parse(contents: &str) -> Result<(v4::Notebook, Value)> {
    let mut document: Value = serde_json::from_str(contents)?;
    let notebook = match nbformat::parse_notebook(contents)? {
        Notebook::V3(notebook) => nbformat::upgrade_v3_notebook(notebook)?,
        Notebook::V4(notebook) => notebook,
        Notebook::V4QuirksMode(notebook) => notebook.notebook().clone(),
        Notebook::Legacy(notebook) => nbformat::upgrade_legacy_notebook(notebook)?,
        _ => anyhow::bail!("Unsupported Jupyter notebook version"),
    };

    if document["nbformat"].as_u64() == Some(4) {
        // Preserve metadata, attachments, and output formats that the editor cannot render.
        document["nbformat_minor"] = json!(document["nbformat_minor"].as_u64().unwrap_or(0).max(5));
        let cells = document["cells"].as_array_mut().context("Missing notebook cells")?;
        anyhow::ensure!(
            cells.len() == notebook.cells.len(),
            "Some notebook cells could not be read"
        );
        for (data, cell) in cells.iter_mut().zip(&notebook.cells) {
            data["id"] = json!(cell.id().as_str());
        }
    } else {
        document = serde_json::from_str(&nbformat::serialize_notebook(&Notebook::V4(notebook.clone()))?)?;
    }
    Ok((notebook, document))
}

pub(super) fn update_source(cell: &mut Value, source: &str) {
    let unchanged = match &cell["source"] {
        Value::String(original) => original == source,
        Value::Array(lines) => lines.iter().filter_map(Value::as_str).collect::<String>() == source,
        _ => false,
    };
    if !unchanged {
        cell["source"] = json!(source.split_inclusive('\n').collect::<Vec<_>>());
    }
}

pub(super) fn execution_output(content: &JupyterMessageContent) -> Option<Value> {
    Some(match content {
        JupyterMessageContent::StreamContent(value) => json!({
            "output_type": "stream", "name": value.name, "text": value.text
        }),
        JupyterMessageContent::ErrorOutput(value) => json!({
            "output_type": "error", "ename": value.ename, "evalue": value.evalue, "traceback": value.traceback
        }),
        JupyterMessageContent::DisplayData(value) => json!({
            "output_type": "display_data", "data": value.data, "metadata": value.metadata
        }),
        JupyterMessageContent::ExecuteResult(value) => json!({
            "output_type": "execute_result", "data": value.data, "metadata": value.metadata,
            "execution_count": value.execution_count
        }),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_source_preserves_metadata_and_outputs() {
        let original = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {"custom": {"configuration": true}},
            "cells": [{
                "id": "a-code-cell",
                "cell_type": "code",
                "metadata": {"tags": ["keep"]},
                "source": ["1 + 2"],
                "execution_count": 1,
                "outputs": [{
                    "output_type": "display_data",
                    "data": {"text/plain": "3", "application/x-custom": {"result": 3}},
                    "metadata": {"custom": true}
                }]
            }]
        });
        let (_, mut document) = parse(&original.to_string()).unwrap();
        assert_eq!(document, original);
        update_source(&mut document["cells"][0], "2\n+ 3");
        let mut expected = original;
        expected["cells"][0]["source"] = json!(["2\n", "+ 3"]);
        assert_eq!(document, expected);
        assert!(parse(&document.to_string()).is_ok());
    }

    #[test]
    fn kernel_stream_is_saved_as_notebook_output() {
        let message = JupyterMessageContent::StreamContent(jupyter_protocol::StreamContent::stdout("hello\n"));
        let output = execution_output(&message).unwrap();
        assert_eq!(output["output_type"], "stream");
        assert_eq!(output["name"], "stdout");
        assert!(serde_json::from_value::<v4::Output>(output).is_ok());
    }
}
