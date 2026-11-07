package agentstatus

import (
	"bufio"
	"encoding/json"
	"io"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/version"
)

// mcpProtocol is the MCP revision offered to a client that asks for none
// this server knows.
const mcpProtocol = "2025-06-18"

type rpcRequest struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id,omitempty"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

type rpcResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Result  any             `json:"result,omitempty"`
	Error   *rpcError       `json:"error,omitempty"`
}

// setTaskDescription is the set_task tool's description, which is what
// tells the model when to use it.
const setTaskDescription = "Set the one-line task Hangar shows for this environment, so the person can tell " +
	"their environments apart and see what you are working on. Call it when you start on a piece of work " +
	"and whenever the work changes. A few words, like a commit's title: \"Fix rounding in checkout totals\"."

// ServeMCP is an MCP server on stdio, one JSON-RPC message a line, with one
// tool, set_task, which sets the environment's task through the guest
// agent on socket.
func ServeMCP(in io.Reader, out io.Writer, socket string) error {
	enc := json.NewEncoder(out)
	r := bufio.NewReaderSize(in, 64<<10)
	for {
		line, err := r.ReadBytes('\n')
		if len(strings.TrimSpace(string(line))) > 0 {
			var req rpcRequest
			if jerr := json.Unmarshal(line, &req); jerr != nil {
				enc.Encode(rpcResponse{JSONRPC: "2.0", ID: json.RawMessage("null"),
					Error: &rpcError{Code: -32700, Message: "parse error"}})
			} else if req.ID != nil {
				result, rerr := handleMCP(req, socket)
				resp := rpcResponse{JSONRPC: "2.0", ID: req.ID, Result: result, Error: rerr}
				if err := enc.Encode(resp); err != nil {
					return err
				}
			}
		}
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
	}
}

func handleMCP(req rpcRequest, socket string) (any, *rpcError) {
	switch req.Method {
	case "initialize":
		var p struct {
			ProtocolVersion string `json:"protocolVersion"`
		}
		json.Unmarshal(req.Params, &p)
		v := p.ProtocolVersion
		if v == "" {
			v = mcpProtocol
		}
		return map[string]any{
			"protocolVersion": v,
			"capabilities":    map[string]any{"tools": map[string]any{}},
			"serverInfo":      map[string]any{"name": "hangar", "version": version.String()},
		}, nil
	case "ping":
		return map[string]any{}, nil
	case "tools/list":
		return map[string]any{"tools": []any{map[string]any{
			"name":        "set_task",
			"description": setTaskDescription,
			"inputSchema": map[string]any{
				"type": "object",
				"properties": map[string]any{
					"task": map[string]any{"type": "string", "description": "The task, in a few words."},
				},
				"required": []string{"task"},
			},
		}}}, nil
	case "tools/call":
		var p struct {
			Name      string `json:"name"`
			Arguments struct {
				Task string `json:"task"`
			} `json:"arguments"`
		}
		if err := json.Unmarshal(req.Params, &p); err != nil || p.Name != "set_task" {
			return nil, &rpcError{Code: -32602, Message: "no such tool"}
		}
		text := strings.TrimSpace(p.Arguments.Task)
		if text == "" {
			return toolResult("The task needs some text.", true), nil
		}
		reply, err := ask(socket, Request{SetTask: &text}, taskWait+5*time.Second)
		if err != nil {
			return toolResult("Hangar could not set the task: "+err.Error(), true), nil
		}
		if reply.Task == nil {
			return toolResult("Hangar has no task for this environment.", false), nil
		}
		return toolResult("The environment's task is \""+reply.Task.Text+"\".", false), nil
	}
	return nil, &rpcError{Code: -32601, Message: "method not found"}
}

func toolResult(text string, isError bool) map[string]any {
	return map[string]any{"content": []any{map[string]any{"type": "text", "text": text}}, "isError": isError}
}
