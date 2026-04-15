# K.R.I.A. Safety & Guardrail Specification
# Version 2.0.0 — April 2026

## Overview

This document defines the complete safety policy for K.R.I.A.'s OS interaction layer,
internet connectivity, file operations, and automation engine.
Every tool call passes through this policy engine before execution.

---

## Risk Classification Matrix

### Tier 0 — GREEN (Auto-Execute)

Actions that are read-only, trivially reversible, or purely informational.

```yaml
green_actions:
  # App Control
  - open_application
  - list_running_apps
  - focus_window
  
  # System Info (read-only)
  - get_cpu_usage
  - get_memory_info
  - get_disk_space
  - get_network_status
  - get_battery_status
  - get_gpu_info
  - get_system_uptime
  
  # File Reading
  - read_file          # Non-system paths only
  - search_files
  - list_directory
  - get_file_info
  - calculate_dir_size
  
  # Document Parsing (read-only)
  - parse_pdf
  - parse_docx
  - parse_xlsx
  - parse_csv
  - summarize_document
  
  # Internet (read-only, no data exfiltration)
  - web_search
  - fetch_webpage
  - get_weather
  - get_news
  - get_stock_price
  - check_url_status
  - rss_feed_read
  - get_public_ip
  
  # Network Diagnostics
  - ping_host
  - dns_lookup
  - traceroute
  - get_active_connections
  - get_wifi_networks
  - speed_test
  
  # Clipboard (read)
  - get_clipboard
  - clipboard_history
  - transform_clipboard
  
  # UI
  - screenshot
  - lock_screen
  
  # Knowledge (read)
  - recall_fact
  - list_remembered
  - search_knowledge
  - get_snippet
  - list_snippets
  
  # Notifications
  - send_notification
  - compose_email       # Drafts only, does NOT send
  - open_email_draft
  - schedule_reminder
  
  # Automation (read)
  - list_workflows
  - list_scheduled_tasks
  - list_macros
  
  # Plugins (read)
  - list_plugins
  
  # Memory
  - remember_fact
  - ingest_document
  - save_snippet
  
  # Environment (read)
  - get_environment_variable
  - list_environment_variables
  - get_power_plan
```

### Tier 1 — YELLOW (Execute + Notify)

Actions that modify user-level state but are easily reversible.

```yaml
yellow_actions:
  # App Control
  - close_application
  - kill_process         # User processes only, not system services
  
  # System Config
  - set_volume
  - set_brightness
  - toggle_wifi
  - set_power_plan
  - connect_wifi
  
  # File Modification
  - write_file           # Non-system paths, max 10MB
  - create_directory
  - rename_file
  - copy_file
  
  # Document Conversion
  - convert_document
  
  # Internet (write-ish)
  - download_file        # Modifies disk, needs size awareness
  
  # Clipboard (write)
  - set_clipboard
  - type_text
  
  # Package (informational)
  - update_application   # Single package update
  
  # Power
  - sleep
  - hibernate
  
  # Plugins
  - enable_plugin
  - disable_plugin
  
  # Automation
  - run_workflow         # User-defined only
  - record_macro
  - replay_macro
```

### Tier 2 — RED (Block Until Approved)

Actions that modify system state, are difficult to reverse, or involve elevated privileges.

```yaml
red_actions:
  # File Destruction
  - delete_file
  - delete_directory
  - move_file            # When destination is system path
  
  # System Administration
  - manage_service       # Start/stop/restart services
  - set_environment_variable
  - add_to_path
  - edit_shell_profile
  - manage_firewall_rule
  
  # OS Control
  - shutdown_system
  - reboot_system
  - clean_temp_files
  
  # Package Management
  - install_application
  - uninstall_application
  - update_all_packages
  
  # Code Execution
  - execute_python       # Arbitrary code
  - execute_bash         # Arbitrary code
  - execute_powershell   # Arbitrary code
  
  # Scheduled Tasks
  - create_scheduled_task
  - delete_scheduled_task
  - modify_scheduled_task
  
  # Registry (Windows)
  - write_registry
  
  # Plugins
  - install_plugin
  - uninstall_plugin
  
  # Dangerous combos
  - set_process_priority # System processes
  - change_network_config
```

### Tier 3 — BLACK (Always Denied)

Actions that could cause irreversible system damage. Hardcoded, cannot be overridden.

```yaml
black_patterns:
  # Disk destruction
  - "format [a-z]:"
  - "diskpart.*clean"
  - "cipher /w"
  - "mkfs\\."
  - "dd if=.*/dev/"
  
  # Boot/system integrity
  - "bcdedit"
  - "bootrec"
  - "sfc /scannow.*delete"
  - "grub-install"
  
  # Security disabling
  - "netsh.*firewall.*disable"
  - "Set-MpPreference.*Disable.*True"
  - "net stop WinDefend"
  - "ufw disable"
  - "iptables -F"
  - "setenforce 0"
  
  # System file destruction
  - "del.*system32"
  - "rmdir.*windows"
  - "rm -rf /"
  - "rm -rf /*"
  - "Remove-Item.*-Recurse.*C:\\Windows"
  - "rm -rf /boot"
  - "rm -rf /etc"
  - "rm -rf /usr"
  
  # Credential theft attempts
  - "mimikatz"
  - "lsass"
  - "SAM.*dump"
  - "sekurlsa"
  - "/etc/shadow"
  - "passwd.*dump"
  
  # Reverse shells / remote access
  - "nc -.*-e"
  - "ncat.*-e"
  - "bash -i >& /dev/tcp"
  - "python.*socket.*connect"
  
  # Cryptocurrency mining
  - "xmrig"
  - "minerd"
  - "cgminer"
```

---

## Path-Based Escalation Rules

Any tool targeting these paths is automatically escalated to RED:

```yaml
protected_paths:
  # Windows
  - "C:\\Windows\\**"
  - "C:\\Program Files\\**"
  - "C:\\Program Files (x86)\\**"
  - "C:\\ProgramData\\**"
  - "C:\\Users\\*\\AppData\\Local\\Microsoft\\**"
  - "C:\\Boot\\**"
  - "**\\System32\\**"
  - "**\\SysWOW64\\**"
  
  # Linux
  - "/etc/**"
  - "/usr/**"
  - "/var/**"
  - "/boot/**"
  - "/sys/**"
  - "/proc/**"
  - "/root/**"
  - "/sbin/**"
  
  # Common sensitive
  - "**/.ssh/**"
  - "**/.gnupg/**"
  - "**/.kria/rollback/**"  # Prevent tampering with rollback data
```

---

## Internet Safety Rules

### Outbound Data Protection

K.R.I.A. NEVER sends the following over the network:
- User files or file contents (unless explicitly downloading/uploading by user request)
- Clipboard contents
- Conversation history or chat logs
- System credentials, SSH keys, or tokens
- Environment variables
- Audit logs

### Download Safety

```yaml
download_rules:
  max_file_size_mb: 500
  max_concurrent_downloads: 3
  allowed_protocols: ["https", "http"]  # HTTPS preferred
  blocked_extensions_for_execution: [".sh", ".py", ".ps1", ".bat", ".cmd", ".exe", ".msi", ".deb", ".rpm"]
  auto_scan: false  # No auto-execution of downloaded content
  require_approval_for_execution: true  # RED tier to run downloaded scripts
```

### Request Logging

Every outgoing HTTP request is logged:

```yaml
internet_audit_fields:
  - timestamp
  - url
  - method (GET/POST/HEAD)
  - response_status
  - response_size_bytes
  - cached (true/false)
  - session_id
```

### Domain Controls

```yaml
# Optional: ~/.kria/internet_policy.yml
blocked_domains: []           # User can add domains to block
allowed_domains: []           # If non-empty, acts as allowlist
https_only: true              # Reject plain HTTP (configurable)
rate_limit_per_domain: 60     # Max requests/minute per domain
```

---

## HITL Approval Protocol

### Voice Approval Flow

```
KRIA: "I need your approval to delete 3 files in /home/obaid/Documents/old/.
       This is classified as a RED action. Say 'approve' to proceed or 'deny' to cancel."
USER: "Approve"
KRIA: "Creating a restore point... Done. Executing deletion. 3 files removed successfully."
```

### GUI Approval Flow

WebSocket message sent to dashboard:

```json
{
  "type": "hitl_request",
  "id": "req_abc123",
  "action": "delete_file",
  "parameters": {
    "paths": ["/home/obaid/Documents/old/file1.txt", "..."]
  },
  "risk_level": "RED",
  "description": "Delete 3 files in Documents/old",
  "timeout_seconds": 30,
  "rollback_available": true
}
```

### Timeout Behavior

| Scenario | Timeout | Action |
|---|---|---|
| RED action, no response | 30 seconds | Auto-DENY |
| YELLOW action, no response | N/A | Auto-EXECUTE (non-blocking) |
| Multiple pending requests | Queue | Process in order, extend timeout |

---

## Rollback Specification

### Rollback Storage

```
~/.kria/rollback/
├── 2026-04-11T14-30-00/
│   ├── manifest.json         # What was changed
│   ├── files/                # Backed up files
│   │   ├── old_file1.txt
│   │   └── old_file2.txt
│   └── registry/             # Registry exports
│       └── HKCU_Software_Foo.reg
├── 2026-04-11T15-00-00/
│   └── ...
```

### manifest.json Schema

```json
{
  "timestamp": "2026-04-11T14:30:00Z",
  "session_id": "sess_abc123",
  "action": "delete_file",
  "risk_level": "RED",
  "changes": [
    {
      "type": "file_deleted",
      "original_path": "/home/obaid/Documents/old/file1.txt",
      "backup_path": "files/old_file1.txt",
      "hash_sha256": "a1b2c3..."
    }
  ],
  "rollback_command": "restore_files",
  "expires": "2026-04-14T14:30:00Z"
}
```

### Retention Policy

- Default retention: **72 hours**
- Maximum storage: **5 GB** (oldest snapshots pruned first)
- User can adjust via config

---

## Audit Log Schema

```sql
CREATE TABLE IF NOT EXISTS audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    session_id  TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    parameters  TEXT    NOT NULL,  -- JSON serialized
    risk_level  TEXT    NOT NULL CHECK (risk_level IN ('GREEN', 'YELLOW', 'RED', 'BLACK')),
    decision    TEXT    NOT NULL CHECK (decision IN ('AUTO_EXECUTED', 'APPROVED', 'DENIED', 'BLOCKED', 'TIMEOUT')),
    decided_by  TEXT    NOT NULL CHECK (decided_by IN ('POLICY', 'USER_VOICE', 'USER_GUI', 'TIMEOUT', 'HARDCODED')),
    result      TEXT             CHECK (result IN ('SUCCESS', 'FAILED', 'ROLLED_BACK', NULL)),
    error_msg   TEXT,
    rollback_id TEXT,
    duration_ms INTEGER,
    network_url TEXT             -- URL if this was an internet request
);

CREATE INDEX idx_audit_timestamp ON audit_log(timestamp);
CREATE INDEX idx_audit_session   ON audit_log(session_id);
CREATE INDEX idx_audit_risk      ON audit_log(risk_level);
CREATE INDEX idx_audit_action    ON audit_log(action);
```

---

## Automation Safety

### Workflow Execution Limits

| Limit | Value | Reason |
|---|---|---|
| Max steps per workflow | 50 | Prevent infinite loops |
| Max execution time per workflow | 5 minutes | Prevent runaway processes |
| Max concurrent workflows | 3 | Resource protection |
| Step timeout | 30 seconds per step | Prevent hangs |
| Only user-created workflows | TRUE | No auto-generated workflows without approval |

### Plugin Safety

| Rule | Enforcement |
|---|---|
| Plugins cannot modify safety engine | Hardcoded isolation |
| Plugins cannot access other plugins' data | Isolated storage per plugin |
| Plugin tools go through policy engine | Same as built-in tools |
| Plugin installation requires RED approval | User must confirm |
| Plugins cannot bypass HITL | Gateway is above plugin layer |

---

## Emergency Stop

Voice command: **"KRIA, emergency stop"** or **"KRIA, halt all"**

This immediately:
1. Kills all running tool subprocesses
2. Cancels all pending HITL requests
3. Flushes the action queue
4. Stops all active workflows
5. Stops all scheduled tasks
6. Logs the emergency stop event
7. Enters safe mode (only GREEN actions allowed until manually reset)
