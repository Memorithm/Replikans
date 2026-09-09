#!/usr/bin/env python3
"""MCP stdio bridge. Financial authorization and persistence stay in Rust.

Python 3.11+, standard library only, POSIX. No shell, model, key or live venue.
One bounded child invocation per tool call; never retry an uncertain operation.
"""
import argparse
import json
import math
import os
import re
import selectors
import subprocess
import sys
import time

PROTOCOL = "2025-11-25"
MAX_INPUT = 1_048_576
MAX_OUTPUT = 8_388_608


def obj(properties=None, required=None):
    properties = properties or {}
    return {"type": "object", "properties": properties,
            "required": list(properties) if required is None else required,
            "additionalProperties": False}


TEXT = {"type": "string", "minLength": 1, "maxLength": 1024}
DECIMAL = {"type": "string", "pattern": r"^(0|[1-9][0-9]*)(\.[0-9]{1,18})?$",
           "maxLength": 60}
TIME = {"type": "integer", "minimum": 0, "maximum": 9223372036854775807}
ORDER_TYPE = {"oneOf": [{"enum": ["Market"]}, obj({"Limit": obj({"price": DECIMAL})})]}
INTENT = obj({
    **{key: TEXT for key in ("intent_id", "idempotency_key", "client_order_id",
                            "agent_id", "decision_id", "strategy_version")},
    "rationale": {"type": "string", "minLength": 1, "maxLength": 16384},
    "evidence_refs": {"type": "array", "items": TEXT, "minItems": 1, "maxItems": 128},
    "created_at_ms": TIME, "expires_at_ms": TIME,
    "request": obj({"instrument_id": TEXT, "side": {"enum": ["Buy", "Sell"]},
                    "order_type": ORDER_TYPE, "quantity": DECIMAL,
                    "tif": {"enum": ["Gtc"]}, "reduce_only": {"enum": [False]},
                    "post_only": {"enum": [False]}, "rules_version": TEXT}),
    "reference": obj({"venue": {"enum": ["paper"]}, "instrument_id": TEXT,
                      "price": DECIMAL, "observed_at_ms": TIME, "valid_until_ms": TIME}),
})
ID = obj({"client_order_id": TEXT})


def tool(name, description, schema, readonly=False, idempotent=False):
    return {"name": name, "description": description, "inputSchema": schema,
            "annotations": {"readOnlyHint": readonly, "destructiveHint": not readonly,
                            "idempotentHint": readonly or idempotent, "openWorldHint": False}}


TOOLS = [
    tool("capabilities", "Discover actual paper runtime capabilities; no live trading.", obj(), True),
    tool("instrument_rules", "Read operator-configured exact instrument rules.",
         obj({"instrument_id": TEXT}), True),
    tool("order_prepare", "Persist a complete decision and reserve balances; does not submit.",
         obj({"intent": INTENT}), idempotent=True),
    tool("order_submit", "Dispatch a previously prepared client_order_id ONCE. On uncertainty query/reconcile; never resubmit.", ID),
    tool("order_cancel", "Request cancellation; inspect resulting order state. Not an undo of fills.", ID),
    tool("order_get", "Read local order state; does not query venue. Use execution_reconcile for venue receipts.", ID, True),
    tool("execution_reconcile", "Query venue receipts and persist them; absence never authorizes resubmission.", ID),
    tool("account_snapshot", "Read exact balances, fills and unresolved recovery identities.", obj(), True),
    tool("session_export", "Export journal decisions and receipts. Bounded response; no key material.", obj(), True),
]
BY_NAME = {item["name"]: item for item in TOOLS}


class Invalid(ValueError):
    pass


def validate(value, schema, path="arguments"):
    """Validate exactly the JSON Schema subset used in this static registry."""
    if "oneOf" in schema:
        matches = 0
        for choice in schema["oneOf"]:
            try:
                validate(value, choice, path)
                matches += 1
            except Invalid:
                pass
        if matches != 1:
            raise Invalid(path + ": expected exactly one supported variant")
        return
    if "enum" in schema and not any(type(value) is type(v) and value == v for v in schema["enum"]):
        raise Invalid(path + ": unsupported value")
    kind = schema.get("type")
    expected = {"object": dict, "array": list, "string": str, "integer": int}
    if kind and type(value) is not expected[kind]:
        raise Invalid(path + ": wrong JSON type")
    if kind == "object":
        if set(value) - set(schema["properties"]) or set(schema["required"]) - set(value):
            raise Invalid(path + ": missing or unknown fields")
        for key, item in value.items():
            validate(item, schema["properties"][key], path + "." + key)
    elif kind in ("string", "array"):
        suffix = "Length" if kind == "string" else "Items"
        if not schema.get("min" + suffix, 0) <= len(value) <= schema.get("max" + suffix, MAX_INPUT):
            raise Invalid(path + ": length outside budget")
        if kind == "string" and "pattern" in schema and not re.fullmatch(schema["pattern"], value):
            raise Invalid(path + ": expected plain exact decimal string")
        if kind == "array":
            for item in value:
                validate(item, schema["items"], path + "[]")
    elif kind == "integer" and not schema["minimum"] <= value <= schema["maximum"]:
        raise Invalid(path + ": integer outside range")


def strict_json(data):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise Invalid("duplicate JSON field")
            result[key] = value
        return result

    def constant(_):
        raise Invalid("nonfinite JSON number")

    return json.loads(data, object_pairs_hook=pairs, parse_constant=constant)


class BackendError(Exception):
    pass


class RustBackend:
    def __init__(self, command, timeout=15.0, output_limit=MAX_OUTPUT):
        self.command = command
        self.timeout = timeout
        self.output_limit = output_limit

    def call(self, command):
        payload = (json.dumps(command, allow_nan=False, separators=(",", ":")) + "\n").encode()
        if len(payload) > MAX_INPUT:
            raise BackendError("runtime input budget exceeded")
        # Child diagnostics are deliberately not returned to the model.
        try:
            child = subprocess.Popen(self.command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, shell=False)
        except OSError as error:
            raise BackendError("runtime could not start") from error
        output = bytearray()
        deadline = time.monotonic() + self.timeout
        try:
            with selectors.DefaultSelector() as selector:
                os.set_blocking(child.stdin.fileno(), False)
                os.set_blocking(child.stdout.fileno(), False)
                selector.register(child.stdin, selectors.EVENT_WRITE)
                selector.register(child.stdout, selectors.EVENT_READ)
                sent = 0
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise BackendError("runtime timeout; outcome may be unknown; query/reconcile before any new action")
                    for key, _ in selector.select(remaining):
                        if key.fileobj is child.stdin:
                            sent += os.write(child.stdin.fileno(), payload[sent:sent + 65536])
                            if sent == len(payload):
                                selector.unregister(child.stdin)
                                child.stdin.close()
                        else:
                            chunk = os.read(child.stdout.fileno(), 65536)
                            if not chunk:
                                selector.unregister(child.stdout)
                            elif len(output) + len(chunk) > self.output_limit:
                                raise BackendError("runtime output budget exceeded; outcome may be unknown; inspect/reconcile")
                            else:
                                output.extend(chunk)
            remaining = deadline - time.monotonic()
            if remaining <= 0 or child.wait(timeout=remaining) != 0:
                raise BackendError("runtime failed; outcome may be unknown; inspect/reconcile")
            response = strict_json(output)
            if type(response) is not dict or type(response.get("ok")) is not bool:
                raise BackendError("invalid runtime response; outcome may be unknown")
            if not response["ok"]:
                raise BackendError(str(response.get("error", "runtime rejected command")))
            if "result" not in response:
                raise BackendError("missing runtime result; outcome may be unknown")
            return response["result"]
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            raise BackendError("runtime communication failed; outcome may be unknown; query/reconcile") from error
        finally:
            if child.poll() is None:
                child.kill()
            child.wait()
            child.stdin.close()
            child.stdout.close()


def rpc_error(identifier, code, message):
    return {"jsonrpc": "2.0", "id": identifier, "error": {"code": code, "message": message}}


class Server:
    def __init__(self, backend):
        self.backend = backend
        self.state = "new"

    def handle(self, message):
        if (type(message) is not dict or message.get("jsonrpc") != "2.0"
                or type(message.get("method")) is not str
                or set(message) - {"jsonrpc", "id", "method", "params"}):
            return rpc_error(None, -32600, "Invalid Request")
        identifier = message.get("id")
        notification = "id" not in message
        if not notification and type(identifier) not in (str, int):
            return rpc_error(None, -32600, "Invalid request id")
        method = message["method"]
        params = message.get("params", {})
        if type(params) is not dict:
            return None if notification else rpc_error(identifier, -32602, "Expected object params")
        if notification:
            if method == "notifications/initialized" and self.state == "initializing" and not params:
                self.state = "ready"
            # Notifications MUST NOT execute tool calls or produce responses.
            return None
        try:
            if method == "initialize":
                if self.state != "new":
                    raise Invalid("already initialized")
                if (type(params.get("protocolVersion")) is not str
                        or type(params.get("capabilities")) is not dict
                        or type(params.get("clientInfo")) is not dict
                        or any(type(params["clientInfo"].get(k)) is not str for k in ("name", "version"))):
                    raise Invalid("invalid initialization parameters")
                self.state = "initializing"
                result = {"protocolVersion": PROTOCOL, "capabilities": {"tools": {"listChanged": False}},
                          "serverInfo": {"name": "replikan-trading", "version": "0.1.0"},
                          "instructions": "Paper only. Prepare before submit. Uncertain results require query/reconcile, never blind retries. Operator policy is enforced by Rust."}
            elif method == "ping":
                validate(params, obj())
                result = {}
            elif self.state != "ready":
                return rpc_error(identifier, -32002, "Initialize and send notifications/initialized first")
            elif method == "tools/list":
                validate(params, obj())
                result = {"tools": TOOLS}
            elif method == "tools/call":
                if set(params) - {"name", "arguments", "_meta"} or type(params.get("name")) is not str:
                    raise Invalid("invalid tool call")
                if "_meta" in params and type(params["_meta"]) is not dict:
                    raise Invalid("invalid metadata")
                name = params["name"]
                if name not in BY_NAME:
                    raise Invalid("unknown tool")
                try:
                    arguments = params.get("arguments", {})
                    validate(arguments, BY_NAME[name]["inputSchema"])
                    value = self.invoke(name, arguments)
                    result = {"content": [{"type": "text", "text": json.dumps(value, allow_nan=False)}],
                              "structuredContent": value, "isError": False}
                except (Invalid, BackendError) as error:
                    result = {"content": [{"type": "text", "text": str(error)}], "isError": True}
            else:
                return rpc_error(identifier, -32601, "Method not found")
            return {"jsonrpc": "2.0", "id": identifier, "result": result}
        except Invalid as error:
            return rpc_error(identifier, -32602, str(error))

    def invoke(self, name, arguments):
        operations = {"capabilities": "capabilities", "order_prepare": "prepare",
                      "order_submit": "dispatch", "order_cancel": "cancel",
                      "execution_reconcile": "reconcile", "account_snapshot": "snapshot",
                      "session_export": "export", "instrument_rules": "export", "order_get": "snapshot"}
        forwarded = {} if name in ("instrument_rules", "order_get") else arguments
        value = self.backend.call({"operation": operations[name], **forwarded})
        if type(value) is not dict:
            raise BackendError("runtime result is not an object")
        if name == "capabilities":
            if value.get("live") is not False or value.get("mode") != "paper":
                raise BackendError("bridge requires explicit paper-only capabilities")
            value = {**value, "protocol": "mcp-stdio", "protocol_version": PROTOCOL,
                     "tools": list(BY_NAME), "model_client": False}
        elif name == "instrument_rules":
            rules = value.get("config", {}).get("instruments", {}).get(arguments["instrument_id"])
            if rules is None:
                raise BackendError("unknown configured instrument")
            value = {"mode": "paper", "rules": rules}
        elif name == "order_get":
            order = next((order for order in value.get("orders", [])
                          if order.get("client_order_id") == arguments["client_order_id"]), None)
            if order is None:
                raise BackendError("unknown local order")
            value = {"mode": "paper", "order": order, "journal_hash": value["journal_hash"]}
        return value


def serve(server, reader, writer):
    while True:
        line = reader.readline(MAX_INPUT + 1)
        if not line:
            return
        if len(line) > MAX_INPUT:
            raise Invalid("MCP input budget exceeded; closing transport")
        try:
            request = strict_json(line)
            response = server.handle(request)
        except (ValueError, UnicodeError, RecursionError):
            response = rpc_error(None, -32700, "Parse error")
        if response is not None:
            encoded = (json.dumps(response, allow_nan=False, separators=(",", ":")) + "\n").encode()
            writer.write(encoded)
            writer.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", required=True, help="operator-selected replikan-trading executable")
    parser.add_argument("--config", required=True)
    parser.add_argument("--journal", required=True)
    parser.add_argument("--paper-venue", required=True)
    parser.add_argument("--timeout", type=float, default=15.0)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 60:
        parser.error("timeout must be finite and in (0, 60]")
    backend = RustBackend([os.path.abspath(args.runtime), os.path.abspath(args.config),
                           os.path.abspath(args.journal), os.path.abspath(args.paper_venue)], args.timeout)
    try:
        serve(Server(backend), sys.stdin.buffer, sys.stdout.buffer)
    except (OSError, Invalid):
        print("MCP transport closed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
