#![allow(dead_code)]

const SERVER_LOOP: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"method":"initialize"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"fake\",\"version\":\"0\"}}}";;
    *'"method":"tools/list"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"echo\",\"inputSchema\":{\"type\":\"object\"}}]}}";;
    *'"method":"tools/call"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"pong\"}]}}";;
  esac
done
"#;

pub fn fake_server(prelude: &str) -> String {
    format!("#!/bin/sh\ndir=$(dirname \"$0\")\n{prelude}\n{SERVER_LOOP}")
}
