# K.R.I.A. Project Structure
# ===========================
# Kernel-Responsive Intelligent Agent — Complete AI Assistant
# Developer: Obaidullah Zeeshan
# Version: 2.0.0

KRIA/
├── docs/                          # 📄 Documentation
│   ├── SYSTEM_DESIGN_DOCUMENT.md  # Master system design (BTech submission)
│   ├── IMPLEMENTATION_GUIDE.md    # Phased implementation plan
│   ├── SAFETY_SPECIFICATION.md    # Guardrail & policy spec
│   ├── PROJECT_STRUCTURE.md       # This file — directory layout
│   ├── HOW_TO_RUN.md             # Setup and running instructions
│   ├── SPEECH_RECOGNITION.md     # Speech pipeline deep-dive
│   └── QUERIES.md                # Design queries, decisions & FAQ
│
├── docker/                        # 🐳 Containerized deployment
│   ├── docker-compose.yml         # Full stack orchestration
│   ├── docker-compose.gpu.yml     # GPU override
│   ├── brain/                     # GPU container (LLM + STT)
│   │   ├── Dockerfile
│   │   ├── entrypoint.sh
│   │   └── configs/
│   │       └── llama.yml
│   ├── voice/                     # Audio pipeline container
│   │   └── Dockerfile
│   ├── core/                      # Orchestrator container
│   │   └── Dockerfile
│   ├── data/                      # Persistence container (Redis + ChromaDB)
│   │   └── Dockerfile
│   ├── dashboard/                 # Web UI container
│   │   └── Dockerfile
│   ├── init/                      # Model downloader (first boot)
│   │   └── Dockerfile
│   └── secrets/
│       └── bridge_secret.txt      # Bridge daemon shared secret
│
├── src/                           # 🐍 Python source (kria-core)
│   ├── kria/
│   │   ├── __init__.py
│   │   ├── main.py                # FastAPI app entry
│   │   │
│   │   ├── agent/                 # 🧠 ReAct agent loop
│   │   │   ├── __init__.py
│   │   │   ├── loop.py            # Core ReAct loop
│   │   │   ├── llm_client.py      # llama.cpp API client
│   │   │   ├── router.py          # Intent classifier
│   │   │   ├── planner.py         # Multi-step planner
│   │   │   └── prompts.py         # System prompts
│   │   │
│   │   ├── tools/                 # 🔧 Tool registry + 65+ implementations
│   │   │   ├── __init__.py        # Auto-import all tool modules
│   │   │   ├── registry.py        # Tool registration framework
│   │   │   │
│   │   │   ├── # --- App & Process Control ---
│   │   │   ├── app_control.py     # open, close, focus, list apps
│   │   │   ├── app_lifecycle.py   # install, uninstall, update via package mgr
│   │   │   ├── process_mgmt.py    # kill, priority, process details
│   │   │   │
│   │   │   ├── # --- File & Document Intelligence ---
│   │   │   ├── file_ops.py        # CRUD file operations
│   │   │   ├── document_parser.py # PDF, DOCX, XLSX, CSV, images
│   │   │   ├── document_convert.py# Format conversion via pandoc
│   │   │   ├── file_organizer.py  # Smart rule-based file organization
│   │   │   ├── file_watcher.py    # Directory monitoring (watchdog)
│   │   │   │
│   │   │   ├── # --- System Info & Config ---
│   │   │   ├── system_info.py     # CPU, RAM, disk, network, battery, GPU
│   │   │   ├── system_config.py   # Volume, brightness, WiFi, power plan
│   │   │   ├── service_mgmt.py    # systemctl / sc.exe service control
│   │   │   ├── disk_mgmt.py       # Disk usage, cleanup, duplicates
│   │   │   ├── network_mgmt.py    # ping, DNS, traceroute, WiFi, speed test
│   │   │   ├── power_mgmt.py      # shutdown, reboot, sleep, hibernate
│   │   │   ├── env_mgmt.py        # Environment variables, PATH, profiles
│   │   │   ├── task_scheduler.py  # Cron / Task Scheduler management
│   │   │   │
│   │   │   ├── # --- Internet & Web ---
│   │   │   ├── web_tools.py       # Web search, page fetch, weather
│   │   │   ├── download_mgr.py    # File download with progress
│   │   │   ├── rss_reader.py      # RSS/Atom feed aggregation
│   │   │   ├── api_tools.py       # Generic REST API consumer
│   │   │   │
│   │   │   ├── # --- Code Execution ---
│   │   │   ├── code_executor.py   # Sandboxed Python/Bash/PowerShell
│   │   │   │
│   │   │   ├── # --- Communication ---
│   │   │   ├── notification.py    # Desktop notification dispatch
│   │   │   ├── email_composer.py  # Email drafting (no auto-send)
│   │   │   ├── clipboard_mgr.py   # Clipboard read/write/history/transform
│   │   │   ├── reminder.py        # Timed reminder scheduling
│   │   │   │
│   │   │   ├── # --- Knowledge & Memory ---
│   │   │   ├── knowledge_tools.py # remember/recall/search facts
│   │   │   ├── doc_ingest.py      # Document ingestion → ChromaDB RAG
│   │   │   └── snippet_lib.py     # Code/text snippet library
│   │   │
│   │   ├── voice/                 # 🎙️ Audio pipeline
│   │   │   ├── __init__.py
│   │   │   ├── pipeline.py        # Full voice loop orchestrator
│   │   │   ├── wake_word.py       # OpenWakeWord integration
│   │   │   ├── vad.py             # Silero VAD
│   │   │   ├── stt_client.py      # Whisper.cpp API client
│   │   │   └── tts_client.py      # Piper API client
│   │   │
│   │   ├── safety/                # 🛡️ Guardrail system
│   │   │   ├── __init__.py
│   │   │   ├── policy_engine.py   # 4-tier risk classification
│   │   │   ├── hitl.py            # Human-in-the-loop gateway
│   │   │   ├── rollback.py        # Rollback manager
│   │   │   └── audit.py           # Audit logging
│   │   │
│   │   ├── memory/                # 💾 Context & memory
│   │   │   ├── __init__.py
│   │   │   ├── conversation.py    # Sliding window buffer
│   │   │   ├── persistent.py      # SQLite + FTS5
│   │   │   ├── semantic.py        # ChromaDB RAG
│   │   │   ├── context_manager.py # 3-tier context combiner
│   │   │   └── user_prefs.py      # User preference learning
│   │   │
│   │   ├── automation/            # ⚙️ Workflow & scheduling engine
│   │   │   ├── __init__.py
│   │   │   ├── scheduler.py       # APScheduler-based scheduler
│   │   │   ├── workflow_engine.py # YAML workflow executor
│   │   │   ├── event_bus.py       # System event trigger bus
│   │   │   └── macro_recorder.py  # Action recording & replay
│   │   │
│   │   ├── plugins/               # 🔌 Plugin architecture
│   │   │   ├── __init__.py
│   │   │   ├── manager.py         # Plugin install/enable/disable
│   │   │   ├── api.py             # Plugin API interface
│   │   │   └── loader.py          # Dynamic plugin discovery & loading
│   │   │
│   │   ├── api/                   # 🌐 API layer
│   │   │   ├── __init__.py
│   │   │   ├── routes.py          # REST API endpoints
│   │   │   └── websocket.py       # WebSocket server + HITL broadcast
│   │   │
│   │   └── infra/                 # 🏗️ Infrastructure
│   │       ├── __init__.py
│   │       ├── config.py          # Pydantic settings
│   │       ├── vram_orchestrator.py
│   │       ├── redis_bus.py       # Redis pub/sub + cache
│   │       ├── circuit_breaker.py # Fault tolerance
│   │       ├── supervisor.py      # Supervised task runner
│   │       ├── health.py          # Service health registry
│   │       ├── isolation.py       # Exception isolation decorator
│   │       └── logging_config.py  # Structured JSON logging
│   │
│   └── tests/                     # 🧪 Test suite
│       ├── __init__.py
│       ├── conftest.py            # Shared fixtures
│       ├── test_agent_loop.py
│       ├── test_safety_pipeline.py
│       ├── test_tool_isolation.py
│       ├── test_voice_pipeline.py
│       ├── test_memory_degradation.py
│       ├── test_circuit_breakers.py
│       ├── test_web_tools.py
│       ├── test_file_tools.py
│       ├── test_automation.py
│       ├── test_plugins.py
│       └── test_concurrent.py
│
├── dashboard/                     # ⚛️ React Web Dashboard
│   ├── src/
│   │   ├── App.tsx                # Main app with routing
│   │   ├── components/
│   │   │   ├── Chat.tsx           # Chat interface
│   │   │   ├── HITLModal.tsx      # HITL approval popup
│   │   │   ├── StatusBar.tsx      # Service health indicators
│   │   │   ├── AuditLog.tsx       # Audit log viewer
│   │   │   ├── SystemMonitor.tsx  # CPU/RAM/GPU graphs
│   │   │   ├── FileExplorer.tsx   # File browser + operations
│   │   │   ├── WorkflowEditor.tsx # Visual workflow builder
│   │   │   ├── PluginManager.tsx  # Plugin install/enable UI
│   │   │   ├── Settings.tsx       # User preferences
│   │   │   └── Notifications.tsx  # Notification center
│   │   ├── hooks/
│   │   │   ├── useWebSocket.ts    # WebSocket + reconnect
│   │   │   └── useApi.ts          # REST API client
│   │   └── types/
│   │       └── index.ts           # Shared TypeScript types
│   ├── package.json
│   ├── vite.config.ts
│   └── index.html
│
├── plugins/                       # 🔌 User-installed plugins
│   └── README.md                  # Plugin development guide
│
├── scripts/                       # 🔧 Utility scripts
│   ├── setup.ps1                  # Windows setup
│   ├── setup.sh                   # Linux/WSL setup
│   ├── download_models.py         # Model downloader
│   └── kria_bridge.py             # Host bridge daemon
│
├── diagrams/                      # 📊 Architecture diagrams
│
├── models/                        # 🤖 Downloaded model files
│
├── pyproject.toml
├── README.md
├── .env.example
└── .gitignore
