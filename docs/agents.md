# Integrating AI Coding Agents with Agent Power Guard

Agent Power Guard ensures your machine stays awake during long-running agent workflows (code generation, autonomous debugging, long test runs, heavy builds, or model fine-tuning) without disabling system sleep permanently. As soon as the agent finishes or crashes, Agent Power Guard resumes monitoring and safely puts your PC to sleep when human and system conditions are met.

---

## 3 Integration Options

| Method | Setup Effort | Best For | How It Works |
|---|---|---|---|
| **1. CLI Wrapper (`run`)** | **Zero config** | Any CLI tool, script, or agent | Wraps process execution; binds lease to PID and auto-releases on exit |
| **2. MCP Server (`mcp`)** | Low (paste config) | Claude Code, Cursor, Antigravity | Agent decides when to call `keep_awake` and `release` tools via stdio MCP |
| **3. REST API / Shell Hooks** | Minimal script | Custom pipelines, CI/CD, batch jobs | Direct HTTP calls with API token or CLI commands (`lease`, `release`) |

---

## Method 1: Process Wrapper (`agent-power-guard run`)

The simplest and most resilient way to run any agent or long task:

```bash
# Wrap Claude Code
agent-power-guard run -- claude

# Wrap with custom reason and max safety timeout (e.g. 2 hours)
agent-power-guard run --max-mins 120 --reason "Refactoring auth system" -- claude

# Wrap long build or test suites
agent-power-guard run -- cargo test --workspace

# Wrap Python training / inference
agent-power-guard run --reason "LoRA fine-tuning" -- python train.py
```

### Key Advantages
- **PID-Bound Auto-Release:** Automatically acquires a lease tied to the spawned child's Process ID.
- **Crash-Proof:** If the agent exits, panics, or crashes, the watchdog detects the dead PID within 1–2 seconds and immediately releases the lease.
- **Ctrl+C Propagation:** Pressing `Ctrl+C` terminates the child gracefully and releases the power lease.
- **Exit Code Forwarding:** Preserves and forwards the child's exact exit code.

---

## Method 2: Model Context Protocol (MCP)

Agent Power Guard includes a built-in stdio MCP server: `agent-power-guard mcp`. It exposes tools allowing LLMs to manage power leases directly.

### Exposed Tools

- `keep_awake(minutes, reason)`: Requests a keep-awake lease for $N$ minutes with a descriptive reason.
- `release()`: Immediately releases the active agent lease so the machine can sleep once idle.
- `status()`: Queries live system power status, active countdown, failing blockers, and lease details.
- `postpone(minutes)`: Temporarily defers automated sleep by $N$ minutes.

---

### Claude Code Setup

#### Via Claude CLI:
```bash
claude mcp add agent-power-guard -- agent-power-guard mcp
```

#### Via `~/.claude.json` or `.claude/config.json`:
```json
{
  "mcpServers": {
    "agent-power-guard": {
      "command": "agent-power-guard",
      "args": ["mcp"]
    }
  }
}
```

#### Claude Code Shell Hooks (Alternative):
If you prefer shell hooks in `.bashrc`, `.zshrc`, or PowerShell profiles:
```bash
# Before running a prompt:
agent-power-guard lease 60 "Claude agent running"

# When task is complete:
agent-power-guard release
```

---

### Cursor Setup

1. Open **Cursor Settings** (`Ctrl + ,` or `Cmd + ,`).
2. Navigate to **Features** -> **MCP**.
3. Click **+ Add New MCP Server**.
4. Fill in:
   - **Name:** `agent-power-guard`
   - **Type:** `command`
   - **Command:** `agent-power-guard mcp`

Alternatively, add it directly to `.cursor/mcp.json`:
```json
{
  "mcpServers": {
    "agent-power-guard": {
      "command": "agent-power-guard",
      "args": ["mcp"]
    }
  }
}
```

---

### Antigravity Setup

Add `agent-power-guard` to your Antigravity MCP configuration:
```json
{
  "mcpServers": {
    "agent-power-guard": {
      "command": "agent-power-guard",
      "args": ["mcp"]
    }
  }
}
```

---

## Method 3: Direct CLI & REST API

You can script Agent Power Guard from any shell or language using the CLI or REST API.

### CLI Commands
```bash
# Acquire a 30-minute lease
agent-power-guard lease 30 "Running database migrations"

# Check status
agent-power-guard status

# Release lease
agent-power-guard release

# Postpone sleep by 15 minutes
agent-power-guard postpone 15
```

### Python REST API Example
```python
import os
import requests
from pathlib import Path

# Load auth token automatically across Windows, macOS, and Linux
token_path = Path(os.environ.get("APPDATA", "")) / "AgentPowerGuard" / "api_token.txt"
if not token_path.exists():
    token_path = Path.home() / "Library" / "Application Support" / "AgentPowerGuard" / "api_token.txt"
if not token_path.exists():
    token_path = Path.home() / ".config" / "agent-power-guard" / "api_token.txt"

token = token_path.read_text().strip()
headers = {"Authorization": f"Bearer {token}"}

# Request a 45-minute lease
resp = requests.post(
    "http://127.0.0.1:49281/api/lease",
    params={"mins": 45, "reason": "Autonomous batch task"},
    headers=headers
)
print(resp.json())

# Check status
status = requests.get("http://127.0.0.1:49281/api/status", headers=headers).json()
print("Countdown:", status["countdown_secs"])

# Release lease when finished
requests.post("http://127.0.0.1:49281/api/release", headers=headers)
```

---

## Agent Profiles & Condition 2 (Process Monitoring)

Under **Conditions** -> **Condition 2: Watched Agent Processes**, Agent Power Guard tracks CPU usage of specific agent processes.

### Built-in Profiles
Select built-in profiles directly from the GUI or configure them in `config.json`:
- **Claude Code:** `claude, node`
- **Cursor:** `cursor, node, code`
- **Antigravity:** `antigravity, python, node`
- **Ollama / Local LLM:** `ollama, llama-box, text-generation-launcher`
- **Docker:** `docker, dockerd, com.docker.backend`
- **Rust / Cargo:** `cargo, rustc, rust-analyzer`
- **Node / JS:** `node, bun, deno, pnpm`
- **Python ML:** `python, python3, jupyter, torchrun`

### Noise Filtering (`ignored_processes`)
To prevent lightweight background helper processes from falsely blocking sleep, add them to `ignored_processes`:
- Language server daemon spikes
- Git background indexers
- Telemetry agents

