#!/usr/bin/env python3
"""Mock ACP agent for end-to-end gateway testing.

Speaks line-delimited JSON-RPC 2.0 per the Agent Client Protocol v1:
answers initialize/session/new, runs a session/prompt turn that emits
agent_message_chunk updates, a mid-turn tool call with a permission
request, then finishes with stopReason.
"""
import json
import sys
import time


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def reply(id, result):
    send({"jsonrpc": "2.0", "id": id, "result": result})


def main():
    session_id = None
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        method = msg.get("method")
        id = msg.get("id")
        params = msg.get("params", {})

        if method == "initialize":
            assert msg["params"]["protocolVersion"] == 1
            reply(id, {
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": False},
                "agentInfo": {"name": "mock-agent", "title": "Mock Agent", "version": "0.1.0"},
                "authMethods": [],
            })
        elif method == "session/new":
            assert params.get("cwd"), "session/new must carry cwd"
            session_id = f"sess_mock_{int(time.time() * 1000)}"
            reply(id, {
                "sessionId": session_id,
                "configOptions": [
                    {
                        "id": "model",
                        "name": "Model",
                        "category": "model",
                        "type": "select",
                        "currentValue": "glm-4.6",
                        "options": [
                            {"value": "glm-4.6", "name": "GLM-4.6"},
                            {"value": "glm-4.5-air", "name": "GLM-4.5-Air"},
                        ],
                    },
                ],
            })
        elif method == "session/prompt":
            assert params.get("sessionId") == session_id
            prompt_text = params["prompt"][0]["text"]

            # Tool call needing permission, then two text chunks, then done.
            send({"jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": session_id,
                "update": {"sessionUpdate": "tool_call", "toolCallId": "call_1",
                           "title": "Run mock tool", "kind": "other", "status": "pending"},
            }})
            send({"jsonrpc": "2.0", "id": "perm-1", "method": "session/request_permission",
                  "params": {
                      "sessionId": session_id,
                      "toolCall": {"toolCallId": "call_1"},
                      "options": [
                          {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"},
                          {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                      ],
                  }})

            def handle_permission(perm_result):
                outcome = perm_result["outcome"]
                assert outcome["outcome"] == "selected", f"unexpected outcome {outcome}"
                # The gateway must pick the allow option, not the first listed.
                assert outcome["optionId"] == "allow-once", \
                    f"gateway approved the wrong option: {outcome['optionId']}"
                send({"jsonrpc": "2.0", "method": "session/update", "params": {
                    "sessionId": session_id,
                    "update": {"sessionUpdate": "tool_call_update", "toolCallId": "call_1",
                               "status": "completed"},
                }})
                send({"jsonrpc": "2.0", "method": "session/update", "params": {
                    "sessionId": session_id,
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": "Echo: " + prompt_text}},
                }})
                send({"jsonrpc": "2.0", "method": "session/update", "params": {
                    "sessionId": session_id,
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": " (done)"}},
                }})
                reply(id, {"stopReason": "end_turn"})

            return handle_permission
        else:
            if id is not None:
                reply(id, None)

    return None


if __name__ == "__main__":
    handler = main()
    if handler is None:
        sys.exit(1)
    # Permission reply arrives after session/prompt; drain stdin for it.
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        sys.stderr.write("MOCK-AGENT-RECV: " + json.dumps(msg) + "\n")
        if msg.get("id") == "perm-1" and "result" in msg:
            handler(msg["result"])
            break
        elif msg.get("method") and msg.get("id") is not None:
            reply(msg["id"], None)
