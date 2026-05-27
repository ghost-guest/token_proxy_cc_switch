pub(crate) fn is_codex_tool_call_context_item_type(item_type: &str) -> bool {
    matches!(
        item_type.trim(),
        "tool_call"
            | "function_call"
            | "local_shell_call"
            | "tool_search_call"
            | "custom_tool_call"
            | "mcp_tool_call"
    )
}

pub(crate) fn is_codex_tool_call_output_item_type(item_type: &str) -> bool {
    matches!(
        item_type.trim(),
        "function_call_output"
            | "tool_search_output"
            | "custom_tool_call_output"
            | "mcp_tool_call_output"
    )
}
