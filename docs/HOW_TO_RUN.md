# How to Run K.R.I.A.

This guide covers every way to run the project — with and without Docker, on both Linux and Windows.

K.R.I.A. v2.0 is a **complete AI Assistant** with internet connectivity, document intelligence, OS management, app lifecycle control, automation, and more. The setup is the same as before — just more Python dependencies.

---

## What is K.R.I.A. made of?

Before starting, understand the pieces that get launched:

| Service | What it does | Port |
|---|---|---|
| **kria-brain** | Runs the LLM (Qwen3) and speech-to-text (Whisper) | 8080, 8081 |
| **kria-voice** | Text-to-speech (Piper TTS) | 8082 |
| **kria-core** | Main brain of the app — FastAPI + agent loop | 8000 |
| **kria-data** | Database layer — Redis + ChromaDB | 6379, 8083 |
| **kria-dashboard** | Web UI | 3000 |
| **kria_bridge.py** | Runs **on your host machine** (not in Docker) for OS-level access | 9000 |

> **Important:** `kria_bridge.py` always runs directly on your machine, even when everything else runs in Docker. It gives the AI access to your audio, apps, and OS.

---

## Requirements (all methods)

### Linux
- Python 3.12 or newer — check with `python3 --version`
- Git — `sudo apt install git`
- At least 16 GB RAM
- (Optional) NVIDIA GPU with drivers installed

### Windows
- Python 3.12 or newer from [python.org](https://python.org) — check with `python --version`
- Git from [git-scm.com](https://git-scm.com)
- At least 16 GB RAM
- (Optional) NVIDIA GPU with drivers

---

## Step 0 — Clone and get the code

```bash
git clone <your-repo-url> KRIA
cd KRIA
```

---

## Method 1 — Docker (Recommended)

Docker handles all services automatically. This is the easiest way.

### Extra requirements for Docker
- **Linux:** Docker Engine 27+ and Docker Compose v2
  ```bash
  sudo apt install docker.io docker-compose-plugin
  sudo usermod -aG docker $USER   # then log out and back in
  ```
- **Windows:** [Docker Desktop](https://www.docker.com/products/docker-desktop/) — install and start it, then open a terminal

- **(GPU only):** NVIDIA Container Toolkit
  - Linux: `sudo apt install nvidia-container-toolkit && sudo systemctl restart docker`
  - Windows: Docker Desktop handles this automatically if your GPU drivers are installed

---

### Linux — Docker

**Step 1 — Run the setup script**

This creates the `.env` file, virtualenv, directories, and pulls Docker images.

```bash
bash scripts/setup.sh
```

**Step 2 — Download AI models** (~7 GB total, runs once)

```bash
python3 scripts/download_models.py
```

**Step 3 — Start all containers**

CPU only:
```bash
cd docker
docker compose up -d
```

With GPU (NVIDIA):
```bash
cd docker
docker compose -f docker-compose.yml -f docker-compose.gpu.yml up -d
```

**Step 4 — Start the bridge on your host** (in a separate terminal)

```bash
source .venv/bin/activate
python scripts/kria_bridge.py
```

**Step 5 — Open the app**

- Dashboard: http://localhost:3000
- API docs: http://localhost:8000/docs

---

### Windows — Docker

**Step 1 — Open PowerShell as Administrator and run the setup script**

```powershell
Set-ExecutionPolicy RemoteSigned -Scope CurrentUser   # only needed once
.\scripts\setup.ps1
```

To also download models during setup:
```powershell
.\scripts\setup.ps1 -DownloadModels
```

**Step 2 — Download AI models** (if you skipped `-DownloadModels` above)

```powershell
.\.venv\Scripts\python scripts\download_models.py
```

**Step 3 — Start all containers**

CPU only:
```powershell
cd docker
docker compose up -d
```

With GPU (NVIDIA):
```powershell
cd docker
docker compose -f docker-compose.yml -f docker-compose.gpu.yml up -d
```

**Step 4 — Start the bridge on your host** (in a separate PowerShell window)

```powershell
.\.venv\Scripts\python scripts\kria_bridge.py
```

**Step 5 — Open the app**

- Dashboard: http://localhost:3000
- API docs: http://localhost:8000/docs

---

### Checking Docker status

```bash
# See if all containers are running
docker compose ps

# Watch live logs from all services
docker compose logs -f

# Watch logs from one service (e.g. the core)
docker compose logs -f kria-core

# Stop everything
docker compose down
```

---

## Method 2 — Without Docker (Manual Setup)

Use this if you can't or don't want to use Docker. You will install each dependency yourself.

### Extra requirements (no Docker)

You need Redis and ChromaDB running locally:

**Linux:**
```bash
# Redis
sudo apt install redis-server
sudo systemctl start redis

# ChromaDB (runs as a Python server)
pip install chromadb
chroma run --path ~/.kria/chroma --port 8083
```

**Windows:**
```powershell
# Redis — install via Scoop
winget install Memurai.Memurai    # or download Redis for Windows from GitHub

# ChromaDB
pip install chromadb
chroma run --path "$env:USERPROFILE\.kria\chroma" --port 8083
```

You also need **llama.cpp** and **whisper.cpp** running locally — see their respective GitHub pages to build or download binaries, then start them on ports `8080` and `8081`.

---

### Linux — No Docker

**Step 1 — Run the setup script with Docker skipped**

```bash
SKIP_DOCKER=1 bash scripts/setup.sh
```

**Step 2 — Activate the virtualenv**

```bash
source .venv/bin/activate
```

**Step 3 — Download AI models**

```bash
python3 scripts/download_models.py
```

**Step 4 — Edit `.env`** to point at your local services

Open `.env` and set:

```env
KRIA_LLAMA_API_URL=http://localhost:8080
KRIA_WHISPER_API_URL=http://localhost:8081
KRIA_PIPER_API_URL=http://localhost:8082
KRIA_REDIS_URL=redis://localhost:6379/0
KRIA_CHROMA_URL=http://localhost:8083
KRIA_BRIDGE_URL=http://localhost:9000
```

**Step 5 — Start each service in its own terminal**

Terminal 1 — Redis (if not running as a system service):
```bash
redis-server
```

Terminal 2 — ChromaDB:
```bash
chroma run --path ~/.kria/chroma --port 8083
```

Terminal 3 — Start llama.cpp (example):
```bash
./llama-server -m models/llm/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf --port 8080
```

Terminal 4 — K.R.I.A. core:
```bash
source .venv/bin/activate
python -m kria.main
```

Terminal 5 — Bridge:
```bash
source .venv/bin/activate
python scripts/kria_bridge.py
```

**Step 6 — Open the app**

- API docs: http://localhost:8000/docs

---

### Windows — No Docker

**Step 1 — Run setup without Docker**

```powershell
.\scripts\setup.ps1 -SkipDocker
```

**Step 2 — Activate the virtualenv**

```powershell
.\.venv\Scripts\Activate.ps1
```

**Step 3 — Download AI models**

```powershell
.venv\Scripts\python scripts\download_models.py
```

**Step 4 — Edit `.env`** (same values as Linux above)

**Step 5 — Start each service in its own terminal**

Same pattern as Linux — run Redis, ChromaDB, llama.cpp, then:

PowerShell window 1 — K.R.I.A. core:
```powershell
.\.venv\Scripts\python -m kria.main
```

PowerShell window 2 — Bridge:
```powershell
.\.venv\Scripts\python scripts\kria_bridge.py
```

**Step 6 — Open the app**

- API docs: http://localhost:8000/docs

---

## Environment Variables Reference

The `.env` file controls everything. Here are the most important ones:

| Variable | Default | Description |
|---|---|---|
| **Service URLs** | | |
| `KRIA_LLAMA_API_URL` | `http://localhost:8080` | Where llama.cpp is running |
| `KRIA_WHISPER_API_URL` | `http://localhost:8081` | Where whisper.cpp is running |
| `KRIA_PIPER_API_URL` | `http://localhost:8082` | Where Piper TTS is running |
| `KRIA_REDIS_URL` | `redis://localhost:6379/0` | Redis connection |
| `KRIA_CHROMA_URL` | `http://localhost:8083` | ChromaDB connection |
| `KRIA_BRIDGE_URL` | `http://localhost:9000` | Bridge server address |
| `KRIA_BRIDGE_SECRET` | *(auto-generated)* | Shared secret between core and bridge |
| **Features** | | |
| `KRIA_VOICE_ENABLED` | `false` | Set to `true` to enable voice I/O |
| `KRIA_INTERNET_ENABLED` | `true` | Enable internet tools (web search, downloads, etc.) |
| `KRIA_INTERNET_HTTPS_ONLY` | `true` | Reject plain HTTP requests |
| `KRIA_AUTOMATION_ENABLED` | `true` | Enable workflow/scheduler engine |
| `KRIA_PLUGINS_ENABLED` | `true` | Enable plugin system |
| `KRIA_NOTIFICATIONS_ENABLED` | `true` | Enable desktop notifications |
| **Limits** | | |
| `KRIA_MAX_DOWNLOAD_SIZE_MB` | `500` | Max file download size in MB |
| `KRIA_INTERNET_RATE_LIMIT_PER_MIN` | `60` | Max HTTP requests per minute per domain |
| `KRIA_MAX_SCHEDULED_TASKS` | `50` | Max scheduler jobs |
| **Paths** | | |
| `KRIA_DOWNLOADS_DIR` | `~/Downloads/kria` | Where downloaded files go |
| `KRIA_PLUGINS_DIR` | `~/.kria/plugins` | Plugin installation directory |
| `KRIA_WORKFLOWS_DIR` | `~/.kria/workflows` | Workflow YAML files |

---

## Disk and Memory Usage

| Component | Disk | RAM |
|---|---|---|
| Phi-4-mini-instruct LLM (primary) | ~2.5 GB | ~3–4 GB |
| Qwen2.5-VL-7B LLM (secondary, opt-in) | ~4.7 GB | ~6–8 GB |
| mmproj-F16.gguf (vision projector) | ~1.35 GB | — |
| Whisper large-v3-turbo | ~1.5 GB | ~2 GB |
| Piper TTS voice | ~65 MB | ~200 MB |
| Core + services + tools | — | ~2–3 GB |

Total models: **~10 GB** download (primary only: ~4 GB). Minimum 16 GB RAM recommended.

> **Note:** Document parsing (PyMuPDF, openpyxl, pandas) and web tools (httpx, trafilatura) add ~200 MB to Python dependencies. These are installed automatically by `pip install .` or the setup script.

---

## Troubleshooting

**Containers keep restarting**
```bash
docker compose logs kria-brain
```
Usually means the model file is missing. Run `download_models.py` again.

**`kria-core` unhealthy but `kria-brain` is still starting**
Normal — `kria-brain` can take up to 90 seconds to load the model. Wait for it.

**Bridge says "secret mismatch"**
The secret in `.env` (`KRIA_BRIDGE_SECRET`) must match what `kria_bridge.py` loaded. Re-run the setup script to regenerate them together, or copy `~/.kria/bridge_secret.txt` into `.env` manually.

**`No module named kria`**
You are not in the virtualenv. Run `source .venv/bin/activate` (Linux) or `.\.venv\Scripts\Activate.ps1` (Windows).

**Project is on an NTFS drive (e.g. a shared Windows/Linux disk) — `.venv/bin/activate: No such file or directory`**
NTFS doesn't support Linux symlinks, so a plain `python3 -m venv` creates a broken environment. The setup script handles this automatically with `--copies`. If you already have a broken `.venv`, delete it first then re-run:
```bash
rm -rf .venv
bash scripts/setup.sh
```

**Permission denied running `setup.sh`**
```bash
chmod +x scripts/setup.sh
bash scripts/setup.sh
```

**PowerShell says "running scripts is disabled"**
```powershell
Set-ExecutionPolicy RemoteSigned -Scope CurrentUser
```
