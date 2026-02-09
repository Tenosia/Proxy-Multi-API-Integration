# Proxy-Multi-API-Integration

## Overview

Proxy-Multi-API-Integration is a high-performance proxy server written in Rust. It accepts requests in the Anthropic Messages API format and forwards them to any OpenAI-compatible HTTP API, translating request and response payloads so that Anthropic clients work unchanged against non-Anthropic backends.

Typical use cases: point Claude Code, Claude Desktop, or other Anthropic API clients at OpenRouter, at OpenAI, at Azure OpenAI, or at a local OpenAI-compatible server (e.g. Ollama, LiteLLM). You run the proxy once, set your upstream URL and optional API key, and configure the client to use the proxy as if it were the official Anthropic API. The proxy listens on a configurable port (default 3000), exposes the Anthropic-style `/v1/messages` endpoint, and translates to and from the upstream `/v1/chat/completions` format. It supports streaming (SSE), tool/function calling, system prompts, and extended thinking; when thinking is requested it can route to a different model via environment variables. Configuration is via environment variables or `.env` files, with optional daemon mode on Unix.

## Configuration

### Environment variables

Configuration can be set via environment variables or a `.env` file:

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `UPSTREAM_BASE_URL` | Yes | - | OpenAI-compatible endpoint URL |
| `UPSTREAM_API_KEY` | No* | - | API key for upstream service |
| `PORT` | No | `3000` | Server port |
| `REASONING_MODEL` | No | (uses request model) | Model to use when extended thinking is enabled** |
| `COMPLETION_MODEL` | No | (uses request model) | Model to use for standard requests (no thinking)** |
| `DEBUG` | No | `false` | Enable debug logging (`1` or `true`) |
| `VERBOSE` | No | `false` | Enable verbose logging (`1` or `true`) |

\* Required if your upstream endpoint needs authentication.

\*\* The proxy detects when a request has extended thinking enabled (via the `thinking` parameter) and routes it to `REASONING_MODEL`. Standard requests use `COMPLETION_MODEL`. You can use a more capable model for reasoning and a faster or cheaper model for simple completions. If not set, the model from the client request is used.

### Configuration file locations

The proxy looks for `.env` files in this order:

1. Custom path given with `--config`
2. Current working directory (`./.env`)
3. User home directory (`~/.anthropic-proxy.env`)
4. System-wide config (`/etc/anthropic-proxy/.env`)

If no `.env` is found, it uses environment variables from the shell.

## Usage examples

### With Claude Code

```bash
# Start proxy as daemon and use Claude Code
anthropic-proxy --daemon && ANTHROPIC_BASE_URL=http://localhost:3000 claude

# Or use separate terminals:
# Terminal 1: Start proxy
anthropic-proxy

# Terminal 2: Use Claude Code
ANTHROPIC_BASE_URL=http://localhost:3000 claude
```

### With debug logging

```bash
# Via CLI flag
anthropic-proxy --debug

# Via environment variable
DEBUG=true anthropic-proxy

# Verbose (full request/response bodies)
anthropic-proxy --verbose
```

### With custom config file

```bash
anthropic-proxy --config /path/to/my-config.env

# Or use home directory config
cp .env ~/.anthropic-proxy.env
anthropic-proxy
```

### With custom model overrides

```bash
# Different models for reasoning vs standard completion
UPSTREAM_BASE_URL=https://openrouter.ai/api \
  UPSTREAM_API_KEY=sk-or-... \
  REASONING_MODEL=anthropic/claude-3.5-sonnet \
  COMPLETION_MODEL=anthropic/claude-3-haiku \
  PORT=8080 \
  anthropic-proxy
```

This allows using a stronger model for reasoning and a faster or cheaper one for simple completions.

### Running as daemon

```bash
# Start in background
anthropic-proxy --daemon

# Check status
anthropic-proxy status

# Stop
anthropic-proxy stop

# View logs
tail -f /tmp/anthropic-proxy.log

# Custom PID file
anthropic-proxy --daemon --pid-file ~/.anthropic-proxy.pid
anthropic-proxy stop --pid-file ~/.anthropic-proxy.pid
```

When running as a daemon, logs go to `/tmp/anthropic-proxy.log`.

## Supported features

- Text messages
- System prompts (single and multiple)
- Image content (base64)
- Tool/function calling
- Tool results
- Streaming responses
- Extended thinking mode (automatic model routing)
- Temperature, top_p, top_k
- Stop sequences
- Max tokens

Ensure your upstream model supports tool use if you use this proxy with coding agents like Claude Code.

### Extended thinking mode

The proxy detects the `thinking` parameter (e.g. from Claude Code) and routes those requests to `REASONING_MODEL`. Requests without thinking use `COMPLETION_MODEL`. If these variables are not set, the proxy uses the model from the client request.

## Known limitations

The following Anthropic API features are not supported (Claude Code and similar tools work without them):

- `tool_choice` parameter (always uses `auto`)
- `service_tier` parameter
- `metadata` parameter
- `context_management` parameter
- `container` parameter
- Citations in responses
- `pause_turn` and `refusal` stop reasons
- Message Batches API
- Files API
- Admin API

## Troubleshooting

**Error: `UPSTREAM_BASE_URL is required`**

Set the upstream endpoint URL. Examples:
- OpenRouter: `https://openrouter.ai/api`
- OpenAI: `https://api.openai.com`
- Local: `http://localhost:11434`

**Error: `405 Method Not Allowed`**

`UPSTREAM_BASE_URL` probably ends with `/v1`. Remove it. The proxy adds `/v1/chat/completions` itself.

- Wrong: `https://openrouter.ai/api/v1`
- Correct: `https://openrouter.ai/api`

**Model not found errors**

Set `REASONING_MODEL` and `COMPLETION_MODEL` to override the models from client requests.

## License

MIT License. Copyright (c) 2025 m0n0x41d (Ivan Zakutnii). See the LICENSE file in the repository for details.

## Contributing

Contributions are welcome. Suggested steps:

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run `cargo test && cargo clippy`
5. Submit a pull request
