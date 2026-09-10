# Skill: mcp
Trigger: MCP server configuration, tool calls, or JSON-RPC protocol work
Inputs: mcp.json or server configs
Steps:
1. Validate mcp.json format
2. Connect to server via stdio
3. Check tools/list and schema
Verify: cargo test -p flashagent-tools
Forbidden: bypassing ApprovalGate for non-read-only MCP tools
