/**
 * Workflowy MCP Server - Main Entry Point
 *
 * This file re-exports the MCP server for backward compatibility.
 * The actual implementation is in mcp/server.ts
 */

// Re-export everything from the MCP server
export * from "./mcp/server.js";

// Also run the server if this is the main module
import "./mcp/server.js";
