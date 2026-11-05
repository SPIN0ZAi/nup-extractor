// NUP Extractor Frontend Application (Native IPC + Web Bridge)

let currentContainer = null;
let activePackageIndex = 0;
let treeCache = {}; // dirIndex -> dirData
let expandedDirs = new Set();
let isSearching = false;
let lastExtractedDest = null;

// Native IPC Helper
const isNative = typeof window.chrome !== "undefined" && typeof window.chrome.webview !== "undefined";
let nextRequestId = 1;
const pendingRequests = new Map();

function nativeCall(action, params = {}) {
    if (!isNative) {
        return Promise.reject(new Error("Native WebView2 bridge not available"));
    }
    return new Promise((resolve, reject) => {
        const id = `req_${nextRequestId++}`;
        pendingRequests.set(id, { resolve, reject });
        window.chrome.webview.postMessage({ id, action, ...params });

        setTimeout(() => {
            if (pendingRequests.has(id)) {
                pendingRequests.delete(id);
                reject(new Error("Native call timed out"));
            }
        }, 120000);
    });
}

// Global Native Message Dispatcher
if (isNative) {
    window.chrome.webview.addEventListener("message", (event) => {
        const msg = event.data;
        if (!msg) return;

        // Handle request-response
        if (msg.id && pendingRequests.has(msg.id)) {
            const { resolve, reject } = pendingRequests.get(msg.id);
            pendingRequests.delete(msg.id);
            if (msg.success) {
                resolve(msg.data || msg);
            } else {
                reject(new Error(msg.error || "Operation failed"));
            }
            return;
        }

        // Handle streaming events (both camelCase and PascalCase)
        const type = msg.type || msg.Type;
        const info = msg.info || msg.Info;

        if (type === "extract_progress" && info) {
            onExtractProgress(info);
        } else if (type === "extract_done") {
            onExtractDone(msg);
        } else if (type === "extract_error") {
            onExtractError(msg);
        } else if (type === "verify_progress" && info) {
            onVerifyProgress(info);
        } else if (type === "verify_done") {
            onVerifyDone(info || msg);
        } else if (type === "verify_error") {
            onVerifyError(msg);
        } else if (type === "update_progress" && info) {
            onUpdateProgress(info);
        } else if (type === "update_error") {
            onUpdateError(msg);
        } else if (type === "pack_progress" && info) {
            onPackProgress(info);
        } else if (type === "pack_done") {
            onPackDone(msg);
        } else if (type === "pack_error") {
            onPackError(msg);
        }
    });
}

// DOM Elements
const dropzoneView = document.getElementById("dropzone-view");
const workspaceView = document.getElementById("workspace-view");
const btnOpenFile = document.getElementById("btn-open-file");
const btnDropzoneBrowse = document.getElementById("btn-dropzone-browse");
const btnDropzoneSample = document.getElementById("btn-dropzone-sample");
const btnLoadSample = document.getElementById("btn-load-sample");
const btnCloseContainer = document.getElementById("btn-close-container");

// Game Hero Elements
const gameTitleEl = document.getElementById("game-title");
const gamePublisherEl = document.getElementById("game-publisher");
const gameFilenameEl = document.getElementById("game-filename");
const gameFilesizeEl = document.getElementById("game-filesize");
const badgeVersionEl = document.getElementById("badge-version");
const gameIconEl = document.getElementById("game-icon");
const gameIconFallbackEl = document.getElementById("game-icon-fallback");
const specTitleIdEl = document.getElementById("spec-title-id");
const specBuildIdEl = document.getElementById("spec-build-id");
const specPkgCountEl = document.getElementById("spec-pkg-count");
const specCompressionEl = document.getElementById("spec-compression");

// Tabs & Tree
const tabsContainer = document.getElementById("tabs-container");
const treeViewEl = document.getElementById("tree-view");
const treeSearchInput = document.getElementById("tree-search");
const btnClearSearch = document.getElementById("btn-clear-search");
const currentPkgMetaEl = document.getElementById("current-pkg-meta");

// Extraction Controls
const destDirInput = document.getElementById("dest-dir-input");
const btnBrowseDest = document.getElementById("btn-browse-dest");
const btnStartExtraction = document.getElementById("btn-start-extraction");
const btnQuickExtractAll = document.getElementById("btn-quick-extract-all");
const checkOpenExplorer = document.getElementById("check-open-explorer");
const subtreeSelectorGroup = document.getElementById("subtree-selector-group");
const subtreeSelect = document.getElementById("subtree-select");

// Modals
const extractModal = document.getElementById("extract-modal");
const extractProgressBar = document.getElementById("extract-progress-bar");
const extractModalPct = document.getElementById("extract-modal-pct");
const extractStatsFiles = document.getElementById("extract-stats-files");
const extractStatsBytes = document.getElementById("extract-stats-bytes");
const extractCurrentFile = document.getElementById("extract-current-file");
const extractLogBox = document.getElementById("extract-log-box");
const btnModalOpenFolder = document.getElementById("btn-modal-open-folder");
const btnModalClose = document.getElementById("btn-modal-close");

const verifyModal = document.getElementById("verify-modal");
const btnVerifyContainer = document.getElementById("btn-verify-container");
const verifyProgressBar = document.getElementById("verify-progress-bar");
const verifyModalPct = document.getElementById("verify-modal-pct");
const verifyStatusText = document.getElementById("verify-status-text");
const verifyResultBox = document.getElementById("verify-result-box");
const btnVerifyModalClose = document.getElementById("btn-verify-modal-close");

// Right-Click Context Menu Elements
const fileContextMenu = document.getElementById("file-context-menu");
const ctxFileName = document.getElementById("ctx-file-name");
const ctxExtractFile = document.getElementById("ctx-extract-file");
const ctxCopyPath = document.getElementById("ctx-copy-path");
const ctxCopyHash = document.getElementById("ctx-copy-hash");
let selectedContextFile = null;

// Disk Space Warning Modal Elements
const diskSpaceModal = document.getElementById("disk-space-modal");
const diskSpaceDrive = document.getElementById("disk-space-drive");
const diskSpaceFree = document.getElementById("disk-space-free");
const diskSpaceRequired = document.getElementById("disk-space-required");
const btnDiskSpaceChangeFolder = document.getElementById("btn-disk-space-change-folder");
const btnDiskSpaceContinue = document.getElementById("btn-disk-space-continue");
const btnDiskSpaceCancel = document.getElementById("btn-disk-space-cancel");
let diskSpaceContinueCallback = null;

// Windows Shell Integration Elements
const btnShellSettings = document.getElementById("btn-shell-settings");
const shellSettingsModal = document.getElementById("shell-settings-modal");
const shellStatusIndicator = document.getElementById("shell-status-indicator");
const btnToggleShellIntegration = document.getElementById("btn-toggle-shell-integration");
const btnCloseShellSettings = document.getElementById("btn-close-shell-settings");
let isShellRegistered = false;

// Mode Navigation Elements
const navModeExtract = document.getElementById("nav-mode-extract");
const navModePack = document.getElementById("nav-mode-pack");
const packView = document.getElementById("pack-view");
let activeAppMode = "extract"; // "extract" | "pack"

// Pack Form Elements
const packDropzone = document.getElementById("pack-dropzone");
const packDropzoneTitle = document.getElementById("pack-dropzone-title");
const btnBrowsePackSource = document.getElementById("btn-browse-pack-source");
const packSourcePreview = document.getElementById("pack-source-preview");
const packSourcePath = document.getElementById("pack-source-path");
const packDestInput = document.getElementById("pack-dest-input");
const btnBrowsePackDest = document.getElementById("btn-browse-pack-dest");
const packTitleInput = document.getElementById("pack-title-input");
const packPublisherInput = document.getElementById("pack-publisher-input");
const packTitleIdInput = document.getElementById("pack-titleid-input");
const packVersionInput = document.getElementById("pack-version-input");
const btnStartPack = document.getElementById("btn-start-pack");
let selectedPackSourceDir = null;
let lastPackedOutputFile = null;

// Pack Modal Elements
const packModal = document.getElementById("pack-modal");
const packModalTitle = document.getElementById("pack-modal-title");
const packCurrentFile = document.getElementById("pack-current-file");
const packModalPct = document.getElementById("pack-modal-pct");
const packProgressBar = document.getElementById("pack-progress-bar");
const packStatsFiles = document.getElementById("pack-stats-files");
const packStatsBytes = document.getElementById("pack-stats-bytes");
const packLogBox = document.getElementById("pack-log-box");
const btnPackModalOpenFolder = document.getElementById("btn-pack-modal-open-folder");
const btnPackModalCancel = document.getElementById("btn-pack-modal-cancel");
const btnPackModalClose = document.getElementById("btn-pack-modal-close");

// Initialization
document.addEventListener("DOMContentLoaded", () => {
    setupEventListeners();
    // Check if sample game is available
    if (isNative) {
        nativeCall("inspect", { filePath: "sample_game.nup" })
            .then(() => { btnLoadSample.style.display = "inline-flex"; })
            .catch(() => {});
    } else {
        fetch("/api/inspect", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ file_path: "sample_game.nup" })
        }).then(res => {
            if (res.ok) btnLoadSample.style.display = "inline-flex";
        }).catch(() => {});
    }
});

function setupEventListeners() {
    // Open File
    btnOpenFile.addEventListener("click", browseNativeFile);
    btnDropzoneBrowse.addEventListener("click", browseNativeFile);

    // Sample buttons
    btnDropzoneSample.addEventListener("click", () => loadNup("sample_game.nup"));
    btnLoadSample.addEventListener("click", () => loadNup("sample_game.nup"));

    // Close container
    btnCloseContainer.addEventListener("click", () => {
        currentContainer = null;
        workspaceView.style.display = "none";
        dropzoneView.style.display = "flex";
        treeCache = {};
        expandedDirs.clear();
    });

    // Browse Destination Folder
    btnBrowseDest.addEventListener("click", browseNativeFolder);

    // Extraction Mode Radios
    document.querySelectorAll("input[name='extract-mode']").forEach(radio => {
        radio.addEventListener("change", (e) => {
            if (e.target.value === "subtree") {
                subtreeSelectorGroup.style.display = "flex";
                populateSubtreeOptions();
            } else {
                subtreeSelectorGroup.style.display = "none";
            }
        });
    });

    // Start Extraction Buttons
    btnStartExtraction.addEventListener("click", () => startExtraction());
    btnQuickExtractAll.addEventListener("click", () => {
        document.querySelector("input[name='extract-mode'][value='all']").checked = true;
        subtreeSelectorGroup.style.display = "none";
        startExtraction();
    });

    // Modal buttons
    btnModalClose.addEventListener("click", () => {
        extractModal.classList.remove("active");
    });
    btnModalOpenFolder.addEventListener("click", () => {
        if (lastExtractedDest) {
            openFolder(lastExtractedDest);
        }
    });

    // Verification
    btnVerifyContainer.addEventListener("click", startVerification);
    btnVerifyModalClose.addEventListener("click", () => {
        verifyModal.classList.remove("active");
    });

    // Search filter
    treeSearchInput.addEventListener("input", handleSearch);
    btnClearSearch.addEventListener("click", () => {
        treeSearchInput.value = "";
        btnClearSearch.style.display = "none";
        isSearching = false;
        renderTree();
    });

    // Drag and drop events on dropzone
    dropzoneView.addEventListener("dragover", (e) => {
        e.preventDefault();
        dropzoneView.classList.add("dragover");
    });
    dropzoneView.addEventListener("dragleave", () => {
        dropzoneView.classList.remove("dragover");
    });
    dropzoneView.addEventListener("drop", (e) => {
        e.preventDefault();
        dropzoneView.classList.remove("dragover");
        if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
            const file = e.dataTransfer.files[0];
            if (file.path) {
                loadNup(file.path);
            } else {
                browseNativeFile();
            }
        }
    });

    // Disk Space Modal Listeners
    if (btnDiskSpaceContinue) {
        btnDiskSpaceContinue.addEventListener("click", () => {
            diskSpaceModal.classList.remove("active");
            if (diskSpaceContinueCallback) {
                const cb = diskSpaceContinueCallback;
                diskSpaceContinueCallback = null;
                cb();
            }
        });
    }

    if (btnDiskSpaceChangeFolder) {
        btnDiskSpaceChangeFolder.addEventListener("click", async () => {
            diskSpaceModal.classList.remove("active");
            diskSpaceContinueCallback = null;
            await browseNativeFolder();
        });
    }

    if (btnDiskSpaceCancel) {
        btnDiskSpaceCancel.addEventListener("click", () => {
            diskSpaceModal.classList.remove("active");
            diskSpaceContinueCallback = null;
        });
    }

    // Context Menu Action Listeners
    if (ctxExtractFile) {
        ctxExtractFile.addEventListener("click", async () => {
            hideContextMenu();
            if (!selectedContextFile || !currentContainer) return;
            const nupPath = currentContainer.file_path || currentContainer.filePath;
            if (!nupPath) return;

            if (isNative) {
                try {
                    showToast(`Extracting ${selectedContextFile.name}...`);
                    const res = await nativeCall("extract_single_file", {
                        filePath: nupPath,
                        fileIndex: selectedContextFile.index,
                        fileName: selectedContextFile.name
                    });
                    if (res && res.success) {
                        showToast(`Extracted ${res.fileName || selectedContextFile.name}!`);
                    } else if (res && res.cancelled) {
                        // User cancelled Save As dialog
                    } else {
                        alert("Extraction failed: " + (res?.error || "Unknown error"));
                    }
                } catch (err) {
                    alert("Error extracting file: " + err.message);
                }
            } else {
                showToast("Single-file extract is supported in desktop app mode.");
            }
        });
    }

    if (ctxCopyPath) {
        ctxCopyPath.addEventListener("click", () => {
            hideContextMenu();
            if (!selectedContextFile) return;
            const pathText = selectedContextFile.relPath || selectedContextFile.name;
            navigator.clipboard.writeText(pathText).then(() => {
                showToast(`Copied path: ${pathText}`);
            }).catch(() => {
                showToast("Failed to copy to clipboard");
            });
        });
    }

    if (ctxCopyHash) {
        ctxCopyHash.addEventListener("click", () => {
            hideContextMenu();
            if (!selectedContextFile) return;
            const hash = selectedContextFile.sha256;
            if (hash) {
                navigator.clipboard.writeText(hash).then(() => {
                    showToast("Copied SHA-256 hash!");
                }).catch(() => {
                    showToast("Failed to copy to clipboard");
                });
            } else {
                showToast("No SHA-256 hash available for this file");
            }
        });
    }

    // Dismiss context menu on click outside
    document.addEventListener("click", (e) => {
        if (fileContextMenu && !e.target.closest("#file-context-menu")) {
            hideContextMenu();
        }
    });

    document.addEventListener("contextmenu", (e) => {
        if (fileContextMenu && !e.target.closest(".node-row")) {
            hideContextMenu();
        }
    });

    // Windows Explorer Shell Integration Listeners
    if (btnShellSettings) {
        btnShellSettings.addEventListener("click", () => {
            shellSettingsModal.classList.add("active");
            refreshShellStatus();
        });
    }

    if (btnCloseShellSettings) {
        btnCloseShellSettings.addEventListener("click", () => {
            shellSettingsModal.classList.remove("active");
        });
    }

    if (btnToggleShellIntegration) {
        btnToggleShellIntegration.addEventListener("click", async () => {
            try {
                btnToggleShellIntegration.disabled = true;
                const enable = !isShellRegistered;
                const res = await nativeCall("toggle_shell_integration", { enable });
                if (res && res.success) {
                    showToast(enable ? "Windows Explorer context menu registered!" : "Windows Explorer context menu removed.");
                } else {
                    showToast("Failed to update Windows Explorer settings.");
                }
                await refreshShellStatus();
            } catch (e) {
                showToast("Error: " + e.message);
            } finally {
                btnToggleShellIntegration.disabled = false;
            }
        });
    }

    // Mode Switching
    if (navModeExtract) {
        navModeExtract.addEventListener("click", () => switchAppMode("extract"));
    }
    if (navModePack) {
        navModePack.addEventListener("click", () => switchAppMode("pack"));
    }

    // Pack Source Folder Browse
    if (btnBrowsePackSource) {
        btnBrowsePackSource.addEventListener("click", browsePackSourceFolder);
    }

    // Pack Destination Browse
    if (btnBrowsePackDest) {
        btnBrowsePackDest.addEventListener("click", browsePackDestination);
    }

    // Start Packaging
    if (btnStartPack) {
        btnStartPack.addEventListener("click", startPackaging);
    }

    // Pack Dropzone Events
    if (packDropzone) {
        packDropzone.addEventListener("dragover", (e) => {
            e.preventDefault();
            packDropzone.classList.add("dragover");
        });
        packDropzone.addEventListener("dragleave", () => {
            packDropzone.classList.remove("dragover");
        });
        packDropzone.addEventListener("drop", (e) => {
            e.preventDefault();
            packDropzone.classList.remove("dragover");
            if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
                const item = e.dataTransfer.files[0];
                if (item.path) {
                    setPackSourceFolder(item.path);
                }
            }
        });
    }

    // Pack Modal Buttons
    if (btnPackModalCancel) {
        btnPackModalCancel.addEventListener("click", () => {
            if (isNative) {
                nativeCall("cancel_pack").catch(() => {});
            }
            packModal.classList.remove("active");
        });
    }

    if (btnPackModalClose) {
        btnPackModalClose.addEventListener("click", () => {
            packModal.classList.remove("active");
        });
    }

    if (btnPackModalOpenFolder) {
        btnPackModalOpenFolder.addEventListener("click", () => {
            if (lastPackedOutputFile) {
                const folder = lastPackedOutputFile.substring(0, Math.max(lastPackedOutputFile.lastIndexOf("\\"), lastPackedOutputFile.lastIndexOf("/")));
                openFolder(folder || lastPackedOutputFile);
            }
        });
    }
}

// Context Menu Helpers
function showContextMenu(x, y, fileData) {
    if (!fileContextMenu) return;
    selectedContextFile = fileData;
    if (ctxFileName) {
        ctxFileName.textContent = fileData.name || "Selected File";
    }

    fileContextMenu.style.display = "flex";
    fileContextMenu.classList.add("show");

    const menuWidth = 220;
    const menuHeight = 150;
    const posX = Math.min(x, window.innerWidth - menuWidth - 10);
    const posY = Math.min(y, window.innerHeight - menuHeight - 10);

    fileContextMenu.style.left = `${Math.max(10, posX)}px`;
    fileContextMenu.style.top = `${Math.max(10, posY)}px`;
}

function hideContextMenu() {
    if (fileContextMenu) {
        fileContextMenu.classList.remove("show");
        fileContextMenu.style.display = "none";
    }
}

// Disk Space Warning Helper
function showDiskSpaceModal(info, onContinue) {
    diskSpaceContinueCallback = onContinue;
    if (diskSpaceDrive) diskSpaceDrive.textContent = info.driveLetter || "Target Drive";
    if (diskSpaceFree) diskSpaceFree.textContent = info.freeFormatted || formatBytes(info.freeBytes);
    if (diskSpaceRequired) diskSpaceRequired.textContent = info.requiredFormatted || formatBytes(info.requiredBytes);
    if (diskSpaceModal) diskSpaceModal.classList.add("active");
}

// Windows Explorer Shell Integration Helper
async function refreshShellStatus() {
    if (!shellStatusIndicator || !btnToggleShellIntegration) return;
    if (!isNative) {
        shellStatusIndicator.textContent = "Status: Available only in native Windows desktop app";
        btnToggleShellIntegration.disabled = true;
        return;
    }
    try {
        const res = await nativeCall("get_shell_integration_status");
        isShellRegistered = !!(res && res.registered);
        if (isShellRegistered) {
            shellStatusIndicator.innerHTML = 'Status: <span style="color: var(--switch-green); font-weight: 700;">Active in Windows Explorer</span>';
            btnToggleShellIntegration.textContent = "Remove from Explorer";
            btnToggleShellIntegration.className = "btn btn-ghost";
            btnToggleShellIntegration.style.color = "var(--switch-red)";
        } else {
            shellStatusIndicator.innerHTML = 'Status: <span style="color: var(--text-muted);">Not Registered</span>';
            btnToggleShellIntegration.textContent = "Enable Integration";
            btnToggleShellIntegration.className = "btn btn-cyan";
            btnToggleShellIntegration.style.color = "";
        }
    } catch (e) {
        shellStatusIndicator.textContent = "Status check error: " + e.message;
    }
}

// Mode Switcher Helper
function switchAppMode(mode) {
    activeAppMode = mode;
    if (mode === "extract") {
        if (navModeExtract) navModeExtract.classList.add("active");
        if (navModePack) navModePack.classList.remove("active");
        if (packView) packView.style.display = "none";

        if (currentContainer) {
            dropzoneView.style.display = "none";
            workspaceView.style.display = "block";
        } else {
            dropzoneView.style.display = "flex";
            workspaceView.style.display = "none";
        }
    } else if (mode === "pack") {
        if (navModePack) navModePack.classList.add("active");
        if (navModeExtract) navModeExtract.classList.remove("active");
        dropzoneView.style.display = "none";
        workspaceView.style.display = "none";
        if (packView) packView.style.display = "block";
    }
}

// Pack Helpers
async function browsePackSourceFolder() {
    if (!isNative) {
        showToast("Folder browsing available in desktop app");
        return;
    }
    try {
        const res = await nativeCall("browse_pack_folder");
        if (res && res.path) {
            setPackSourceFolder(res.path, res.name);
        }
    } catch (e) {
        showToast("Error choosing folder: " + e.message);
    }
}

function setPackSourceFolder(folderPath, folderName) {
    selectedPackSourceDir = folderPath;
    const name = folderName || folderPath.split(/[\\/]/).pop() || "Custom Game";

    if (packSourcePath) packSourcePath.textContent = folderPath;
    if (packSourcePreview) packSourcePreview.style.display = "block";
    if (packDropzoneTitle) packDropzoneTitle.textContent = "Folder: " + name;

    if (packTitleInput && (!packTitleInput.value || packTitleInput.value.trim() === "")) {
        packTitleInput.value = name;
    }

    if (packDestInput && (!packDestInput.value || packDestInput.value.trim() === "")) {
        packDestInput.value = folderPath + ".nup";
    }
}

async function browsePackDestination() {
    if (!isNative) {
        showToast("Save file dialog available in desktop app");
        return;
    }
    try {
        const defaultName = (packTitleInput && packTitleInput.value.trim()) || "game";
        const res = await nativeCall("browse_pack_output", { defaultName });
        if (res && res.path && packDestInput) {
            packDestInput.value = res.path;
        }
    } catch (e) {
        showToast("Error selecting destination: " + e.message);
    }
}

async function startPackaging() {
    if (!selectedPackSourceDir) {
        alert("Please select a game source folder to pack!");
        return;
    }

    const outputPath = packDestInput ? packDestInput.value.trim() : "";
    if (!outputPath) {
        alert("Please select an output .NUP destination file path!");
        return;
    }

    const titleName = (packTitleInput && packTitleInput.value.trim()) || "Custom Game";
    const publisher = (packPublisherInput && packPublisherInput.value.trim()) || "Homebrew / Mod";
    const titleId = (packTitleIdInput && packTitleIdInput.value.trim()) || "0100000000001000";
    const version = (packVersionInput && packVersionInput.value.trim()) || "1.0.0";
    const compRadio = document.querySelector("input[name='pack-compression']:checked");
    const compressionLevel = compRadio ? parseInt(compRadio.value, 10) : 3;

    lastPackedOutputFile = outputPath;

    // Reset Modal
    if (packModalPct) packModalPct.textContent = "0%";
    if (packProgressBar) packProgressBar.style.width = "0%";
    if (packCurrentFile) packCurrentFile.textContent = "Scanning directory structure...";
    if (packStatsFiles) packStatsFiles.textContent = "0 / 0 files";
    if (packStatsBytes) packStatsBytes.textContent = "0 B / 0 B";
    if (packLogBox) packLogBox.innerHTML = "";
    if (btnPackModalOpenFolder) btnPackModalOpenFolder.style.display = "none";
    if (btnPackModalClose) btnPackModalClose.style.display = "none";
    if (btnPackModalCancel) btnPackModalCancel.style.display = "inline-flex";

    if (packModal) packModal.classList.add("active");
    addLogEntry(packLogBox, `Starting packaging for "${titleName}"...`, "info");

    if (isNative) {
        try {
            await nativeCall("start_pack", {
                sourceDir: selectedPackSourceDir,
                outputPath: outputPath,
                titleName: titleName,
                publisher: publisher,
                titleId: titleId,
                version: version,
                compressionLevel: compressionLevel
            });
        } catch (e) {
            onPackError({ error: e.message });
        }
    } else {
        alert("Packaging is supported in desktop app mode.");
    }
}

function onPackProgress(info) {
    if (!info) return;
    const pct = Math.round(info.percent || 0);

    if (packModalPct) packModalPct.textContent = `${pct}%`;
    if (packProgressBar) packProgressBar.style.width = `${pct}%`;

    if (packCurrentFile && info.currentFile) {
        packCurrentFile.textContent = info.currentFile;
    }

    if (packStatsFiles && info.totalFiles) {
        packStatsFiles.textContent = `${info.filesPacked} / ${info.totalFiles} files`;
    }

    if (packStatsBytes) {
        const procStr = formatBytes(info.bytesProcessed || 0);
        const totStr = formatBytes(info.totalBytes || 0);
        const speed = info.speedFormatted ? ` [${info.speedFormatted}]` : "";
        const ratio = info.compressionRatio > 0 ? ` (${info.compressionRatio}% saved)` : "";
        packStatsBytes.textContent = `${procStr} / ${totStr}${ratio}${speed}`;
    }

    if (packLogBox && info.currentFile && info.status === "packing") {
        if (info.filesPacked % 10 === 0 || info.filesPacked === info.totalFiles) {
            addLogEntry(packLogBox, `[PACK] ${info.currentFile}`, "success");
        }
    }
}

function onPackDone(msg) {
    if (packModalPct) packModalPct.textContent = "100%";
    if (packProgressBar) packProgressBar.style.width = "100%";
    if (packCurrentFile) packCurrentFile.textContent = "NUP Container built successfully!";

    if (btnPackModalOpenFolder) btnPackModalOpenFolder.style.display = "inline-flex";
    if (btnPackModalClose) btnPackModalClose.style.display = "inline-flex";
    if (btnPackModalCancel) btnPackModalCancel.style.display = "none";

    if (packLogBox) {
        addLogEntry(packLogBox, "Container generated and verified cleanly!", "success");
    }

    showToast("NUP Container built successfully!");
}

function onPackError(msg) {
    if (packCurrentFile) packCurrentFile.textContent = "Packaging Error: " + (msg.error || "Unknown error");
    if (btnPackModalClose) btnPackModalClose.style.display = "inline-flex";
    if (btnPackModalCancel) btnPackModalCancel.style.display = "none";
    if (packLogBox) {
        addLogEntry(packLogBox, "ERROR: " + (msg.error || "Packaging failed"), "error");
    }
}

// Native File Picker
async function browseNativeFile() {
    try {
        if (isNative) {
            const res = await nativeCall("browse_file");
            if (res.path) {
                loadNup(res.path);
            }
        } else {
            const res = await fetch("/api/browse/file");
            const data = await res.json();
            if (data.path) {
                loadNup(data.path);
            }
        }
    } catch (e) {
        showToast("Error opening file dialog: " + e.message);
    }
}

// Native Folder Picker
async function browseNativeFolder() {
    try {
        if (isNative) {
            const res = await nativeCall("browse_folder");
            if (res.path) {
                destDirInput.value = res.path;
            }
        } else {
            const res = await fetch("/api/browse/folder");
            const data = await res.json();
            if (data.path) {
                destDirInput.value = data.path;
            }
        }
    } catch (e) {
        showToast("Error opening folder dialog: " + e.message);
    }
}

// Open folder in Explorer
function openFolder(folderPath) {
    if (!folderPath) return;
    try {
        if (isNative) {
            window.chrome.webview.postMessage({ action: "open_folder", path: folderPath });
        } else {
            fetch("/api/open-folder", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ path: folderPath })
            });
        }
    } catch (e) {
        showToast("Could not open folder: " + e.message);
    }
}

// Load and inspect a .NUP container
async function loadNup(filePath) {
    showToast("Parsing NUP container...");
    try {
        let data;
        if (isNative) {
            data = await nativeCall("inspect", { filePath });
        } else {
            const res = await fetch("/api/inspect", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ file_path: filePath })
            });
            if (!res.ok) {
                const err = await res.json();
                throw new Error(err.detail || "Failed to parse NUP container");
            }
            data = await res.json();
        }

        currentContainer = data;
        treeCache = {};
        expandedDirs.clear();
        activePackageIndex = 0;

        renderContainer();
        const title = (data.primary_meta && data.primary_meta.title_name) ||
                      (data.primaryMeta && data.primaryMeta.titleName) || "Game";
        showToast(`Loaded: ${title}`);
    } catch (err) {
        alert("Error loading .NUP file: " + err.message);
    }
}

// Render loaded container data in UI
function renderContainer() {
    dropzoneView.style.display = "none";
    workspaceView.style.display = "block";

    const c = currentContainer;
    const meta = c.primary_meta || c.primaryMeta || {};

    const titleName = meta.title_name || meta.titleName || c.base_name || c.baseName || "Unknown Game";
    const publisher = meta.publisher || "Nintendo Switch Dump";
    const fileName = c.file_name || c.fileName || "";
    const fileSizeFormatted = c.file_size_formatted || c.fileSizeFormatted || formatBytes(c.file_size || c.fileSize);
    const dispVer = meta.display_version || meta.displayVersion || "1.0.0";
    const icon = meta.icon || "";

    gameTitleEl.textContent = titleName;
    gamePublisherEl.textContent = publisher;
    gameFilenameEl.textContent = fileName;
    gameFilesizeEl.textContent = fileSizeFormatted;
    badgeVersionEl.textContent = `v${dispVer}`;

    if (icon && icon.length > 0) {
        gameIconEl.src = icon;
        gameIconEl.style.display = "block";
        gameIconFallbackEl.style.display = "none";
    } else {
        gameIconEl.style.display = "none";
        gameIconFallbackEl.style.display = "block";
    }

    const packages = c.packages || [];
    const basePkg = packages.find(p => p.kind === 0) || packages[0] || {};
    const titleId = basePkg.title_id || basePkg.titleId || "0100000000000000";
    const buildId = basePkg.build_id || basePkg.buildId || "";

    specTitleIdEl.textContent = titleId;
    specBuildIdEl.textContent = buildId ? buildId.slice(0, 20) + "..." : "N/A";
    if (buildId) {
        specBuildIdEl.setAttribute("data-full", buildId);
    }

    const stats = c.stats || {};
    const totalFiles = stats.total_files ?? stats.totalFiles ?? 0;
    const compressionRatio = stats.compression_ratio ?? stats.compressionRatio ?? 0;

    specPkgCountEl.textContent = `${packages.length} package${packages.length > 1 ? "s" : ""} (${totalFiles} files)`;
    specCompressionEl.textContent = `${compressionRatio}% saved via Zstd`;

    // Destination directory
    const defaultOut = c.default_dest_dir || c.defaultDestDir || "";
    if (defaultOut && (!destDirInput.value || destDirInput.value.trim() === "")) {
        destDirInput.value = defaultOut;
    }

    renderTabs();
    populateSubtreeOptions();
    renderTree();
}

// Render Package Tabs
function renderTabs() {
    tabsContainer.innerHTML = "";
    const packages = currentContainer.packages || [];

    packages.forEach((pkg, idx) => {
        const btn = document.createElement("button");
        btn.className = `tab-btn ${idx === activePackageIndex ? "active" : ""}`;
        
        let iconSvg = "";
        let kindClass = "tab-badge";
        const kind = pkg.kind ?? 0;
        const kindName = pkg.kind_name || pkg.kindName || "base";
        const fileCount = pkg.file_count ?? pkg.fileCount ?? 0;

        if (kind === 0) {
            kindClass += " codec-zstd";
            iconSvg = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="2" y="6" width="20" height="12" rx="2"></rect></svg>`;
        } else if (kind === 1) {
            iconSvg = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.67"></path></svg>`;
        } else {
            iconSvg = `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"></path></svg>`;
        }

        btn.innerHTML = `
            ${iconSvg}
            <span>${kindName.toUpperCase()}</span>
            <span class="${kindClass}">${fileCount} files</span>
        `;

        btn.addEventListener("click", () => {
            activePackageIndex = idx;
            document.querySelectorAll(".tab-btn").forEach(b => b.classList.remove("active"));
            btn.classList.add("active");
            populateSubtreeOptions();
            renderTree();
        });

        tabsContainer.appendChild(btn);
    });

    updateCurrentPkgMeta();
}

function updateCurrentPkgMeta() {
    const packages = currentContainer.packages || [];
    const pkg = packages[activePackageIndex];
    if (pkg) {
        const titleId = pkg.title_id || pkg.titleId || "";
        const version = pkg.display_version || pkg.displayVersion || "";
        const uncomp = pkg.uncompressed_formatted || pkg.uncompressedFormatted || "";
        currentPkgMetaEl.textContent = `Title ID: ${titleId} | Version: ${version} | Uncompressed: ${uncomp}`;
    }
}

function populateSubtreeOptions() {
    subtreeSelect.innerHTML = "";
    const packages = currentContainer.packages || [];
    const pkg = packages[activePackageIndex];
    if (!pkg) return;

    const hasRomFs = pkg.has_romfs ?? pkg.hasRomFs ?? false;
    const hasExeFs = pkg.has_exefs ?? pkg.hasExeFs ?? false;

    if (hasRomFs) {
        const opt = document.createElement("option");
        opt.value = "romfs";
        opt.textContent = "romfs/ (Game Assets & Data)";
        subtreeSelect.appendChild(opt);
    }
    if (hasExeFs) {
        const opt = document.createElement("option");
        opt.value = "exefs";
        opt.textContent = "exefs/ (Executables & NPDM)";
        subtreeSelect.appendChild(opt);
    }
}

// Render File Tree
async function renderTree() {
    updateCurrentPkgMeta();
    const packages = currentContainer.packages || [];
    const pkg = packages[activePackageIndex];
    if (!pkg) return;

    treeViewEl.innerHTML = `<div style="color: var(--text-muted); padding: 1rem; text-align: center;">Loading folder structure...</div>`;

    const rootDirIndex = pkg.root_dir_index ?? pkg.rootDirIndex ?? 0;
    const rootData = await fetchDirChildren(rootDirIndex);

    treeViewEl.innerHTML = "";
    expandedDirs.add(rootDirIndex);

    if (!rootData || !rootData.children || rootData.children.length === 0) {
        treeViewEl.innerHTML = `<div style="color: var(--text-muted); padding: 1rem;">No files found in this package.</div>`;
        return;
    }

    renderTreeNodes(rootData.children, treeViewEl, 0);
}

function renderTreeNodes(children, parentEl, depth = 0) {
    children.forEach(node => {
        const nodeEl = document.createElement("div");
        nodeEl.className = "tree-node";

        const rowEl = document.createElement("div");
        rowEl.className = "node-row";

        let indentHtml = "";
        for (let i = 0; i < depth; i++) {
            indentHtml += `<span class="node-indent"></span>`;
        }

        if (node.type === "dir") {
            const isExpanded = expandedDirs.has(node.index);
            const chevron = isExpanded
                ? `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="6 9 12 15 18 9"></polyline></svg>`
                : `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><polyline points="9 18 15 12 9 6"></polyline></svg>`;

            const childCount = node.child_count ?? node.childCount ?? 0;
            rowEl.innerHTML = `
                ${indentHtml}
                <span style="color: var(--text-muted);">${chevron}</span>
                <svg class="node-icon" viewBox="0 0 24 24" fill="none" stroke="var(--switch-blue)" stroke-width="2"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path></svg>
                <span class="node-name" style="font-weight: 600;">${node.name || "root"}</span>
                <span class="node-meta">${childCount} items</span>
            `;

            const subContainer = document.createElement("div");
            subContainer.style.display = isExpanded ? "block" : "none";

            rowEl.addEventListener("click", async () => {
                if (expandedDirs.has(node.index)) {
                    expandedDirs.delete(node.index);
                    subContainer.style.display = "none";
                    renderTree();
                } else {
                    expandedDirs.add(node.index);
                    const subData = await fetchDirChildren(node.index);
                    subContainer.innerHTML = "";
                    renderTreeNodes(subData.children, subContainer, depth + 1);
                    subContainer.style.display = "block";
                    renderTree();
                }
            });

            nodeEl.appendChild(rowEl);
            nodeEl.appendChild(subContainer);

            if (isExpanded && treeCache[node.index]) {
                renderTreeNodes(treeCache[node.index].children, subContainer, depth + 1);
            }

        } else {
            const codecClass = node.codec === "zstd" ? "codec-zstd" : "codec-raw";
            const sizeFormatted = node.size_formatted || node.sizeFormatted || formatBytes(node.size);
            rowEl.innerHTML = `
                ${indentHtml}
                <span style="width: 12px;"></span>
                <svg class="node-icon" viewBox="0 0 24 24" fill="none" stroke="var(--text-muted)" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>
                <span class="node-name">${node.name}</span>
                <div class="node-meta">
                    <span class="codec-pill ${codecClass}">${node.codec}</span>
                    <span>${sizeFormatted}</span>
                </div>
            `;
            rowEl.addEventListener("contextmenu", (e) => {
                e.preventDefault();
                showContextMenu(e.clientX, e.clientY, {
                    name: node.name,
                    relPath: node.rel_path || node.relPath || node.name,
                    sha256: node.sha256,
                    index: node.index
                });
            });
            nodeEl.appendChild(rowEl);
        }

        parentEl.appendChild(nodeEl);
    });
}

async function fetchDirChildren(dirIndex) {
    if (treeCache[dirIndex]) {
        return treeCache[dirIndex];
    }
    const filePath = currentContainer.file_path || currentContainer.filePath;
    try {
        let data;
        if (isNative) {
            data = await nativeCall("get_tree", {
                filePath: filePath,
                dirIndex: dirIndex
            });
        } else {
            const res = await fetch(`/api/tree?file_path=${encodeURIComponent(filePath)}&dir_index=${dirIndex}&recursive=false`);
            data = await res.json();
        }
        treeCache[dirIndex] = data;
        return data;
    } catch (e) {
        console.error("Tree fetch error:", e);
        return { children: [] };
    }
}

// Search
let searchTimeout = null;
function handleSearch(e) {
    const q = e.target.value.trim();
    if (!q) {
        btnClearSearch.style.display = "none";
        isSearching = false;
        renderTree();
        return;
    }

    btnClearSearch.style.display = "inline-flex";
    clearTimeout(searchTimeout);
    searchTimeout = setTimeout(async () => {
        isSearching = true;
        treeViewEl.innerHTML = `<div style="color: var(--text-muted); padding: 1rem; text-align: center;">Searching...</div>`;
        const filePath = currentContainer.file_path || currentContainer.filePath;
        try {
            let results;
            if (isNative) {
                const res = await nativeCall("search", { filePath: filePath, query: q });
                results = res.results;
            } else {
                const res = await fetch(`/api/search?file_path=${encodeURIComponent(filePath)}&q=${encodeURIComponent(q)}`);
                const data = await res.json();
                results = data.results;
            }
            renderSearchResults(results, q);
        } catch (e) {
            treeViewEl.innerHTML = `<div style="color: var(--switch-red); padding: 1rem;">Search failed: ${e.message}</div>`;
        }
    }, 200);
}

function renderSearchResults(results, q) {
    treeViewEl.innerHTML = "";
    if (!results || results.length === 0) {
        treeViewEl.innerHTML = `<div style="color: var(--text-muted); padding: 1rem;">No files matching "${q}"</div>`;
        return;
    }

    const header = document.createElement("div");
    header.style.padding = "0.5rem";
    header.style.color = "var(--text-secondary)";
    header.style.fontSize = "0.75rem";
    header.textContent = `Found ${results.length} files matching "${q}":`;
    treeViewEl.appendChild(header);

    results.forEach(file => {
        const row = document.createElement("div");
        row.className = "node-row";
        const codecClass = file.codec === "zstd" ? "codec-zstd" : "codec-raw";
        const sizeFormatted = file.size_formatted || file.sizeFormatted || formatBytes(file.size);
        row.innerHTML = `
            <svg class="node-icon" viewBox="0 0 24 24" fill="none" stroke="var(--text-muted)" stroke-width="2"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"></path><polyline points="14 2 14 8 20 8"></polyline></svg>
            <span class="node-name">${file.name}</span>
            <div class="node-meta">
                <span class="codec-pill ${codecClass}">${file.codec}</span>
                <span>${sizeFormatted}</span>
            </div>
        `;
        row.addEventListener("contextmenu", (e) => {
            e.preventDefault();
            showContextMenu(e.clientX, e.clientY, {
                name: file.name,
                relPath: file.rel_path || file.relPath || file.name,
                sha256: file.sha256,
                index: file.index
            });
        });
        treeViewEl.appendChild(row);
    });
}

// Extraction Execution
async function startExtraction() {
    if (!currentContainer) {
        alert("Please load a .nup container first!");
        return;
    }

    const nupPath = currentContainer.file_path || currentContainer.filePath;
    if (!nupPath) {
        alert("Container file path is missing. Please reload the container.");
        return;
    }

    let dest = destDirInput.value.trim();
    if (!dest) {
        dest = currentContainer.default_dest_dir || currentContainer.defaultDestDir;
        if (dest) destDirInput.value = dest;
    }

    if (!dest) {
        alert("Please select a destination folder!");
        return;
    }

    const mode = document.querySelector("input[name='extract-mode']:checked").value;
    const openWhenDone = checkOpenExplorer.checked;

    // Check disk space if running in native desktop app
    if (isNative) {
        let requiredBytes = 0;
        if (mode === "package" && currentContainer.packages && currentContainer.packages[activePackageIndex]) {
            const pkg = currentContainer.packages[activePackageIndex];
            requiredBytes = pkg.total_uncompressed_bytes || pkg.totalUncompressedBytes || 0;
        } else {
            requiredBytes = currentContainer.total_uncompressed_bytes || currentContainer.totalUncompressedBytes || 0;
        }

        try {
            const spaceInfo = await nativeCall("check_disk_space", { destDir: dest, requiredBytes: requiredBytes });
            if (spaceInfo && !spaceInfo.hasSufficientSpace) {
                showDiskSpaceModal(spaceInfo, () => {
                    executeExtraction(nupPath, dest, mode, openWhenDone);
                });
                return;
            }
        } catch (e) {
            console.warn("Disk space pre-check error:", e);
        }
    }

    executeExtraction(nupPath, dest, mode, openWhenDone);
}

async function executeExtraction(nupPath, dest, mode, openWhenDone) {
    lastExtractedDest = dest;

    // Reset modal
    extractModalPct.textContent = "0%";
    extractProgressBar.style.width = "0%";
    extractStatsFiles.textContent = "Initializing extraction...";
    extractStatsBytes.textContent = "0 B / 0 B";
    extractCurrentFile.textContent = "Starting high-speed extraction...";
    extractLogBox.innerHTML = "";
    btnModalOpenFolder.style.display = "none";
    btnModalClose.style.display = "none";
    extractModal.classList.add("active");

    const payload = {
        action: "extract",
        filePath: nupPath,
        destDir: dest,
        mode: mode,
        openWhenDone: openWhenDone
    };

    if (mode === "package") {
        payload.packageIndex = activePackageIndex;
    } else if (mode === "subtree") {
        payload.packageIndex = activePackageIndex;
        payload.subtreePath = subtreeSelect.value;
    }

    if (isNative) {
        window.chrome.webview.postMessage(payload);
    } else {
        try {
            const res = await fetch("/api/extract", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                    file_path: nupPath,
                    dest_dir: dest,
                    mode: mode,
                    package_index: payload.packageIndex,
                    subtree_path: payload.subtreePath,
                    open_when_done: openWhenDone
                })
            });
            const data = await res.json();
            const evtSource = new EventSource(`/api/extract/progress/${data.task_id}`);
            evtSource.onmessage = (e) => {
                const msg = JSON.parse(e.data);
                if (msg.status === "extracting") onExtractProgress(msg);
                else if (msg.status === "completed" || msg.status === "done") {
                    evtSource.close();
                    onExtractDone({ destDir: dest });
                } else if (msg.status === "error") {
                    evtSource.close();
                    onExtractError({ error: msg.error });
                }
            };
        } catch (err) {
            onExtractError({ error: err.message });
        }
    }
}

// Extraction Callbacks
function onExtractProgress(info) {
    if (!info) return;

    const rawPct = typeof info.percent === "number" ? info.percent :
                   typeof info.Percent === "number" ? info.Percent : 0;
    const current = info.current ?? info.Current ?? 0;
    const total = info.total ?? info.Total ?? 0;
    const bytesWritten = info.bytesWritten ?? info.bytes_written ?? info.BytesWritten ?? 0;
    const totalBytes = info.totalBytes ?? info.total_bytes ?? info.TotalBytes ?? 0;
    const fileName = info.fileName ?? info.file_name ?? info.FileName ?? "";
    const fileSize = info.fileSize ?? info.file_size ?? info.FileSize ?? 0;

    const clampedPct = Math.min(100, Math.max(0, Math.round(rawPct * 10) / 10));

    extractProgressBar.style.width = `${clampedPct}%`;
    extractModalPct.textContent = `${clampedPct}%`;
    extractStatsFiles.textContent = `Extracted ${current} of ${total} files`;
    extractStatsBytes.textContent = `${formatBytes(bytesWritten)} / ${formatBytes(totalBytes)}`;
    extractCurrentFile.textContent = fileName;

    if (fileName && fileName !== "Initializing extraction..." && fileName !== "Done") {
        addLogEntry(extractLogBox, `[OK] ${fileName} (${formatBytes(fileSize)})`);
    }
}

function onExtractDone(msg) {
    extractProgressBar.style.width = "100%";
    extractModalPct.textContent = "100%";
    extractCurrentFile.textContent = "✓ Extraction completed successfully!";
    const dest = (msg && (msg.destDir || msg.dest_dir || msg.DestDir)) || lastExtractedDest || "Destination";
    addLogEntry(extractLogBox, `✓ All files extracted to: ${dest}`, "success");
    btnModalOpenFolder.style.display = "inline-flex";
    btnModalClose.style.display = "inline-flex";
}

function onExtractError(msg) {
    const errText = (msg && (msg.error || msg.Error)) || (msg && msg.info && (msg.info.error || msg.info.Error)) || "Unknown extraction error";
    extractCurrentFile.textContent = `✕ Extraction error: ${errText}`;
    addLogEntry(extractLogBox, `[ERROR] ${errText}`, "error");
    btnModalClose.style.display = "inline-flex";
}

// Verification Execution
async function startVerification() {
    if (!currentContainer) return;

    verifyModalPct.textContent = "0%";
    verifyProgressBar.style.width = "0%";
    verifyStatusText.textContent = "Validating Superblock, TOC integrity, and file SHA-256 checksums...";
    verifyResultBox.innerHTML = "";
    verifyModal.classList.add("active");

    const filePath = currentContainer.file_path || currentContainer.filePath;

    if (isNative) {
        window.chrome.webview.postMessage({
            action: "verify",
            filePath: filePath,
            deep: true
        });
    } else {
        try {
            const res = await fetch("/api/verify", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ file_path: filePath, deep: true })
            });
            const data = await res.json();
            const evtSource = new EventSource(`/api/verify/progress/${data.task_id}`);
            evtSource.onmessage = (e) => {
                const msg = JSON.parse(e.data);
                if (msg.status === "verifying") onVerifyProgress(msg);
                else if (msg.status === "done") {
                    evtSource.close();
                    onVerifyDone(msg.result || msg);
                } else if (msg.status === "error") {
                    evtSource.close();
                    onVerifyError({ error: msg.error });
                }
            };
        } catch (err) {
            onVerifyError({ error: err.message });
        }
    }
}

function onVerifyProgress(info) {
    if (!info) return;
    const rawPct = typeof info.percent === "number" ? info.percent :
                   typeof info.Percent === "number" ? info.Percent : 0;
    const current = info.current ?? info.Current ?? 0;
    const total = info.total ?? info.Total ?? 0;
    const fileName = info.fileName ?? info.file_name ?? info.FileName ?? "";

    const clampedPct = Math.min(100, Math.max(0, Math.round(rawPct * 10) / 10));

    verifyProgressBar.style.width = `${clampedPct}%`;
    verifyModalPct.textContent = `${clampedPct}%`;
    verifyStatusText.textContent = `Verifying: ${fileName} (${current} / ${total})`;
}

function onVerifyDone(info) {
    verifyProgressBar.style.width = "100%";
    verifyModalPct.textContent = "100%";

    const r = info || {};
    const isValid = r.valid ?? r.Valid ?? false;
    const totalChecked = r.totalFilesChecked ?? r.total_files_checked ?? r.TotalFilesChecked ?? 0;
    const problems = r.problems ?? r.Problems ?? [];

    if (isValid) {
        verifyStatusText.textContent = "✓ Container is 100% valid and verified!";
        addLogEntry(verifyResultBox, "✓ Superblock structure: PASS", "success");
        const tocHash = (currentContainer.superblock && (currentContainer.superblock.toc_sha256 || currentContainer.superblock.tocSha256)) || "";
        if (tocHash) {
            addLogEntry(verifyResultBox, `✓ TOC SHA-256 hash (${tocHash.slice(0, 16)}...): PASS`, "success");
        }
        addLogEntry(verifyResultBox, `✓ All ${totalChecked} files SHA-256 integrity: PASS`, "success");
    } else {
        verifyStatusText.textContent = "✕ Integrity problems found:";
        problems.forEach(p => {
            addLogEntry(verifyResultBox, `[PROBLEM] ${p}`, "error");
        });
    }
}

function onVerifyError(msg) {
    const errText = (msg && (msg.error || msg.Error)) || "Verification error";
    verifyStatusText.textContent = `Verification error: ${errText}`;
    addLogEntry(verifyResultBox, `Error: ${errText}`, "error");
}

function addLogEntry(box, text, type = "") {
    const entry = document.createElement("div");
    entry.className = `log-entry ${type}`;
    entry.textContent = text;
    box.appendChild(entry);
    box.scrollTop = box.scrollHeight;
}

function formatBytes(bytes) {
    if (typeof bytes !== "number" || isNaN(bytes) || bytes <= 0) return "0 B";
    if (bytes < 1024) return bytes + " B";
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + " KB";
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + " MB";
    return (bytes / (1024 * 1024 * 1024)).toFixed(2) + " GB";
}

function copyText(elementId) {
    const el = document.getElementById(elementId);
    if (!el) return;
    const text = el.getAttribute("data-full") || el.textContent;
    navigator.clipboard.writeText(text).then(() => {
        showToast("Copied to clipboard!");
    });
}

function showToast(message) {
    const toast = document.getElementById("toast");
    const msg = document.getElementById("toast-message");
    msg.textContent = message;
    toast.classList.add("show");
    setTimeout(() => {
        toast.classList.remove("show");
    }, 2800);
}

// ==========================================
// Software Auto-Updater Implementation
// ==========================================

let pendingUpdateData = null;

const btnCheckUpdates = document.getElementById("btn-check-updates");
const appVersionBadge = document.getElementById("app-version-badge");
const updateModal = document.getElementById("update-modal");
const updateProgressModal = document.getElementById("update-progress-modal");
const updateVersionBadge = document.getElementById("update-version-badge");
const updateReleaseTitle = document.getElementById("update-release-title");
const updateReleaseNotes = document.getElementById("update-release-notes");
const updateReleaseSize = document.getElementById("update-release-size");
const btnUpdateNow = document.getElementById("btn-update-now");
const btnUpdateLater = document.getElementById("btn-update-later");
const btnUpdateIgnore = document.getElementById("btn-update-ignore");
const btnUpdateCancel = document.getElementById("btn-update-cancel");
const updateProgressPct = document.getElementById("update-progress-pct");
const updateProgressBar = document.getElementById("update-progress-bar");
const updateProgressSpeed = document.getElementById("update-progress-speed");
const updateProgressBytes = document.getElementById("update-progress-bytes");
const updateProgressStatus = document.getElementById("update-progress-status");
const updateProgressTitle = document.getElementById("update-progress-title");

async function checkForUpdates(isManual = false) {
    if (!isNative) return;

    if (isManual) {
        showToast("Checking GitHub Releases for updates...");
    }

    try {
        const res = await nativeCall("check_update", { manual: isManual });
        const data = res.data || res;

        if (data.currentVersion && appVersionBadge) {
            appVersionBadge.textContent = "v" + data.currentVersion;
        }

        if (data.updateAvailable && data.downloadUrl) {
            const ignoredVersion = localStorage.getItem("nup_ignored_update_version");
            if (isManual || ignoredVersion !== data.latestVersion) {
                pendingUpdateData = data;
                showUpdateModal(data);
            }
        } else if (isManual) {
            showToast(`You have the latest version (${data.currentVersion ? 'v' + data.currentVersion : 'up to date'})!`);
        }
    } catch (err) {
        console.warn("Update check failed:", err);
        if (isManual) {
            showToast(`Update check failed: ${err.message || err}`);
        }
    }
}

function showUpdateModal(data) {
    if (!updateModal) return;

    updateVersionBadge.textContent = "v" + (data.latestVersion || "new");
    updateReleaseTitle.textContent = data.releaseName || `NUP Extractor v${data.latestVersion}`;
    updateReleaseNotes.textContent = data.releaseNotes || "Performance enhancements, engine optimizations, and bug fixes.";

    if (data.downloadSize > 0) {
        updateReleaseSize.textContent = `Download package: ~${formatBytes(data.downloadSize)}`;
        updateReleaseSize.style.display = "block";
    } else {
        updateReleaseSize.style.display = "none";
    }

    updateModal.classList.add("active");
}

function onUpdateProgress(info) {
    if (!updateProgressModal.classList.contains("active")) {
        updateProgressModal.classList.add("active");
    }

    const rawPct = info.percent ?? info.Percent ?? 0;
    const pct = Math.min(100, Math.max(0, Math.round(rawPct)));

    updateProgressPct.textContent = `${pct}%`;
    updateProgressBar.style.width = `${pct}%`;

    const downloaded = info.bytesDownloaded ?? info.BytesDownloaded ?? 0;
    const total = info.totalBytes ?? info.TotalBytes ?? 0;
    const speed = info.speedFormatted ?? info.SpeedFormatted ?? "";
    const stage = info.stage ?? info.Stage ?? "downloading";
    const msg = info.message ?? info.Message ?? "";

    if (speed) {
        updateProgressSpeed.textContent = speed;
    }
    if (total > 0) {
        updateProgressBytes.textContent = `${formatBytes(downloaded)} / ${formatBytes(total)}`;
    }

    if (msg) {
        updateProgressStatus.textContent = msg;
    }

    if (stage === "ready") {
        updateProgressTitle.textContent = "Restarting...";
        updateProgressStatus.textContent = "Restarting NUP Extractor to finalize update...";
        btnUpdateCancel.style.display = "none";
    }
}

function onUpdateError(msg) {
    const errorText = (msg && (msg.error || msg.Error)) || "Update failed";
    updateProgressStatus.textContent = `Error: ${errorText}`;
    updateProgressStatus.style.color = "var(--switch-red)";
    showToast(`Update error: ${errorText}`);
    btnUpdateCancel.textContent = "Close";
}

if (btnCheckUpdates) {
    btnCheckUpdates.addEventListener("click", () => checkForUpdates(true));
}

if (appVersionBadge) {
    appVersionBadge.addEventListener("click", () => checkForUpdates(true));
}

if (btnUpdateNow) {
    btnUpdateNow.addEventListener("click", () => {
        if (!pendingUpdateData || !pendingUpdateData.downloadUrl) return;

        updateModal.classList.remove("active");
        updateProgressModal.classList.add("active");
        btnUpdateCancel.style.display = "inline-flex";
        btnUpdateCancel.textContent = "Cancel";

        nativeCall("start_update", {
            downloadUrl: pendingUpdateData.downloadUrl
        }).catch(err => {
            onUpdateError({ error: err.message });
        });
    });
}

if (btnUpdateLater) {
    btnUpdateLater.addEventListener("click", () => {
        updateModal.classList.remove("active");
    });
}

if (btnUpdateIgnore) {
    btnUpdateIgnore.addEventListener("click", () => {
        if (pendingUpdateData && pendingUpdateData.latestVersion) {
            localStorage.setItem("nup_ignored_update_version", pendingUpdateData.latestVersion);
        }
        updateModal.classList.remove("active");
        showToast("Update skipped. You won't be reminded about this version.");
    });
}

if (btnUpdateCancel) {
    btnUpdateCancel.addEventListener("click", () => {
        nativeCall("cancel_update").catch(() => {});
        updateProgressModal.classList.remove("active");
    });
}

// Initialize App Version and check for updates silently in the background
if (isNative) {
    nativeCall("get_app_info").then(res => {
        if (res && res.version && appVersionBadge) {
            appVersionBadge.textContent = "v" + res.version;
        }
        if (res && res.initialFilePath) {
            loadNup(res.initialFilePath);
        }
    }).catch(() => {});

    // Silent background check after short delay
    setTimeout(() => {
        checkForUpdates(false);
    }, 2000);
}
