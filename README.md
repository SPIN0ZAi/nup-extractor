# NUP Extractor

<p align="center">
  <img src="app_icon.ico" alt="NUP Extractor Logo" width="96" height="96">
</p>

<p align="center">
  <strong>A high-performance native desktop tool for inspecting, browsing, verifying, and extracting Nintendo Switch 1 & 2 clean-room <code>.nup</code> container archives.</strong>
</p>

<p align="center">
  🌐 <strong>Official Showcase &amp; Live Web Page:</strong> <a href="https://spin0zai.github.io/nup-extractor/"><strong>spin0zai.github.io/nup-extractor</strong></a>
</p>

<p align="center">
  <a href="https://spin0zai.github.io/nup-extractor/"><img src="https://img.shields.io/badge/Website-spin0zai.github.io%2Fnup--extractor-ff2a44?style=for-the-badge&logo=googlechrome&logoColor=white" alt="Live Website"></a>
</p>

<p align="center">
  <a href="https://github.com/SPIN0ZAi/nup-extractor/releases/latest"><img src="https://img.shields.io/badge/Release-Latest-00d4ff.svg?style=flat-square" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-green.svg?style=flat-square" alt="License"></a>
  <a href="#"><img src="https://img.shields.io/badge/Platform-Windows%20x64-0078D6.svg?style=flat-square" alt="Platform"></a>
  <a href="#"><img src="https://img.shields.io/badge/UI-Liquid%20Glass-blueviolet.svg?style=flat-square" alt="UI"></a>
</p>

---

## 📸 Screenshots

<p align="center">
  <img src="pictures/extractor-main-UI.png" alt="NUP Extractor Main UI" width="95%">
  <br>
  <em>Container Workspace — Embedded game artwork, metadata, Title/Build IDs, multi-package tabs, interactive file tree, and extraction controls.</em>
</p>

<br>

<p align="center">
  <img src="pictures/main-UI.png" alt="Welcome Dropzone" width="48%">
  &nbsp;&nbsp;
  <img src="pictures/extractor-progress-UI.png" alt="Extraction Progress" width="48%">
</p>
<p align="center">
  <em>Left: Initial welcome dropzone screen &bull; Right: High-speed extraction modal with live progress & activity log.</em>
</p>

---

## ⚡ Overview

**NUP Extractor** is a standalone 64-bit Windows GUI utility built to handle **NX Package (`.nup`)** containers—a modern clean-room specification designed by [a55uka](https://github.com/a55uka/nup) for archiving unencrypted Nintendo Switch (1 and 2) game dumps.

Rather than relying on console scripts or heavy web runtimes, NUP Extractor is engineered as a **lightweight, zero-dependency native Windows desktop application** featuring:
- **Zero Command Prompt Window**: Launches directly into a sleek Liquid Glass desktop window, with optional **Floating Orb** minimize-to-edge mode.
- **Zero UI Freezing ("Not Responding")**: Employs 64-bit memory-mapped / streaming I/O and background worker threads, keeping the UI smooth at 60+ FPS even when extracting or packing large game dumps.
- **High-Throughput Zstandard Pipeline**: Streaming Zstandard compression and decompression with adaptive buffered I/O, live ETA, and elapsed duration tracking.
- **Drag-and-Drop Extraction**: Drop single or multiple `.nup` files directly onto `NUP_Extractor.exe` or the app dropzone for immediate extraction (Base, Updates, and DLCs), or double-click registered `.nup` files to inspect them first.
- **In-Memory IPC (No Localhost / Sockets)**: Zero network ports, zero socket binding, and no Windows Firewall alerts.

---

## ✨ Features

- **🎨 Modern Switch Glassmorphic UI**: High-contrast obsidian theme with iconic Nintendo Joy-Con neon red and cyan accents, collapsible folder tree, and animated progress gauges.
- **🖼️ Rich Embedded Metadata**: Displays game icons (PNG/JPEG), Title Name, Publisher, Display Version, Title ID, and Build ID (NSO ModuleId) with 1-click clipboard copy buttons.
- **📦 Multi-Package Support**: Inspects all packages bundled inside the container:
  - **Base Game** (`base/`)
  - **Updates & Patches** (`updates/<version>/`)
  - **DLC & Add-ons** (`dlc/<title_id>/<index>/`)
- **📂 Interactive File Explorer**:
  - Live directory tree navigation with file counters and size badges.
  - Per-file compression codec badges (**RAW** vs **Zstd**) and compression ratio stats.
  - Sub-millisecond instant search across thousands of files.
- **⚡ Flexible Extraction Engine**:
  - **Full Game Dump**: Reconstructs the complete game directory hierarchy.
  - **Package Mode**: Extracts only the selected package tab.
  - **Subtree Mode**: Extracts specific folders (e.g. only `romfs/` or `exefs/`).
  - Native Windows File Explorer folder pickers with auto-open upon completion.
- **🛡️ Integrity Verification**:
  - Validates Superblock structure and Table of Contents (TOC) SHA-256 hash.
  - Deep-verifies individual SHA-256 checksums across every file extent.
- **🔄 In-Place Auto-Updater**:
  - Automatic background checks on startup or on-demand via the **Updates** button.
  - User controls: **Update Now**, **Remind Me Later**, or **Skip This Version**.
  - High-speed streaming download with live MB/s progress telemetry and SHA-256 verification.
  - Seamless in-place replacement and automatic restart without leaving behind temporary files or modifying install directories.
- **✨ Liquid Glass & Floating Orb Mode**:
  - Windows Acrylic / Mica glass backdrop with Dark and Light themes.
  - Optional minimize-to-orb edge snapping with live circular progress ring during extraction, verification, and packing.

---

## 🚀 Download & Installation

### Option 1: Direct Download (Recommended)
Download the latest pre-compiled standalone executable from the [Releases](https://github.com/SPIN0ZAi/nup-extractor/releases/latest) page:

1. Download **`NUP_Extractor-windows-x64.zip`** (or standalone **`NUP_Extractor.exe`**).
2. Extract the ZIP anywhere (e.g. Desktop).
3. Double-click **`NUP_Extractor.exe`** to run, or drag & drop `.nup` files directly onto **`NUP_Extractor.exe`** to extract Base, Updates, and DLCs automatically.

*(Single self-contained executable — no external runtime SDKs required!)*

### CLI Quick Usage
```powershell
# Headless extraction (extracts Base, Updates, and DLCs into <dest_dir>)
NUP_Extractor.exe "C:\Games\MyGame.nup" "D:\Extracted\MyGame"

# Batch extract multiple .nup files next to their source folders
NUP_Extractor.exe --extract "C:\Games\Base.nup" "C:\Games\Update.nup"
```

---

## 🛠️ Building from Source

### Prerequisites
- Windows 10 / 11 (64-bit)
- Native build toolchain (`cargo` / MSVC Build Tools)
- *(Optional)* [.NET 10 SDK](https://dotnet.microsoft.com/download) for running the legacy parity suite

### 1. Clone the Repository
```powershell
git clone https://github.com/SPIN0ZAi/nup-extractor.git
cd nup-extractor
```

### 2. Run Automated Tests
```powershell
cargo test --workspace
```

### 3. Build & Publish Standalone Executable
You can run the one-click build script:
```powershell
.\build_native.bat
```

Or build directly with Cargo:
```powershell
cargo build --release -p nup-extractor
```

The resulting standalone `NUP_Extractor.exe` is generated in `target\release\NUP_Extractor.exe` (and copied to the project root by `build_native.bat`).

---

## 🧪 Automated Test Suite

The workspace includes automated unit, golden-parity, IPC contract, and UI state-machine tests across `crates/` and `app/tests/`, plus legacy parity regression tests in `legacy_dotnet/`:

| Suite | Scope |
| :--- | :--- |
| `nup-core` unit & parity tests | Superblock/TOC parsing, RAW & Zstd decompression, multi-package Base/Update/DLC packing, SHA-256 verification, inspect/search contracts |
| `nup-host-logic` unit & golden tests | Floating orb geometry, snap-to-edge positioning, spring morph state machine, persisted settings round-trips |
| `nup-extractor` integration tests | Startup CLI & drag-and-drop parsing, IPC golden contract replay, embedded static assets, HTTP updater & checksum verification |
| `legacy_dotnet` xUnit suite | Legacy container format parity verification |

---

## 👥 Engineering & Contributing

To maintain high engineering standards, all development follows a Kanban board and Git Flow:
- **Git Flow Guide**: See [docs/WORKFLOW.md](docs/WORKFLOW.md) for branch naming, PR checklist, and release cycles.
- **Kanban Board**: Tasks and bugs are tracked via [GitHub Projects](https://github.com/SPIN0ZAi/nup-extractor/projects).
- **Automated CI**: Pull Requests targeting `develop` or `main` are automatically linted, built, packaged, and verified across all test suites before merging.

---

## 🏛️ Architecture

```
nup-extractor/
├── app/                           # Native Desktop Host, WebView2 UI Bridge, Updater & CLI
├── crates/
│   ├── nup-core/                  # Clean-room NUP parser, Zstd streaming engine & packer
│   └── nup-host-logic/            # Floating orb geometry, snap-to-edge & animation state machine
├── static/                        # Embedded UI Assets (HTML5, CSS3 Liquid Glass, JS)
│   ├── index.html                 # Responsive workspace & Floating Orb layout
│   ├── styles.css                 # Liquid Glass styling, dark/light themes & animations
│   ├── ipc.js                     # In-process WebView2 message transport
│   └── app.js                     # IPC event handling, batch drop queue & UI controller
├── tests_golden/                  # IPC & host logic contract fixtures
├── legacy_dotnet/                 # Archived C# .NET reference implementation & xUnit parity suite
├── pictures/                      # Screenshots showcase
├── docs/                          # Format & UI specifications (SPEC.md, WORKFLOW.md, ORB_MODE.md)
├── .github/workflows/             # GitHub Actions CI & automated release pipelines
├── Cargo.toml                     # Workspace manifest
├── build_native.bat               # 1-Click Build & Test script
├── LICENSE                        # MIT License
└── README.md                      # Project documentation
```

---

## 📄 License & Legal Notice

- **Software License**: Distributed under the [MIT License](LICENSE).
- **Format Specification**: NUP format specification by [a55uka](https://github.com/a55uka/nup) (MIT License).
- **Disclaimer**: This is a clean-room interoperability archive extraction tool. It does **not** contain cryptographic keys (`prod.keys`), proprietary Nintendo code, or copyrighted game files. All game dumps must be legally acquired and unencrypted before use.
