import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  Check,
  ChevronLeft,
  ChevronRight,
  FolderOpen,
  ImageIcon,
  Loader2,
  RotateCcw,
  Search,
  Settings,
  Square,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import type { ImageGroup, ImageItem, ScanOptions, ScanResult } from "./types";

type ResultTab = "exact" | "similar" | "blurry";

type ScanProgress = {
  message: string;
  current: number;
  total: number;
};

const pageSize = 27;
const minimumVisibleBlurScore = 1;

const defaultOptions: ScanOptions = {
  includeSubfolders: true,
  detectExactDuplicates: true,
  detectSimilarImages: true,
  detectBlurryImages: true,
};

const tabs: Array<{ id: ResultTab; label: string }> = [
  { id: "exact", label: "完全重複" },
  { id: "similar", label: "類似画像" },
  { id: "blurry", label: "ブレの可能性" },
];

function App() {
  const [folderPath, setFolderPath] = useState("");
  const [options, setOptions] = useState<ScanOptions>(defaultOptions);
  const [result, setResult] = useState<ScanResult | null>(null);
  const [activeTab, setActiveTab] = useState<ResultTab>("exact");
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [isScanning, setIsScanning] = useState(false);
  const [isMoving, setIsMoving] = useState(false);
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [page, setPage] = useState(0);

  const currentGroups = useMemo(() => {
    if (!result) return [];
    if (activeTab === "exact") return result.exactDuplicateGroups;
    if (activeTab === "similar") return result.similarImageGroups;
    return [
      {
        id: "blurry",
        title: "ブレている可能性がある画像",
        items: result.blurryImages.filter(
          (item) => item.blurScore !== null && item.blurScore > minimumVisibleBlurScore,
        ),
      },
    ].filter((group) => group.items.length > 0);
  }, [activeTab, result]);

  const visibleBlurryCount =
    result?.blurryImages.filter(
      (item) => item.blurScore !== null && item.blurScore > minimumVisibleBlurScore,
    ).length ?? 0;

  const pagedGroups = useMemo(
    () => currentGroups.slice(page * pageSize, page * pageSize + pageSize),
    [currentGroups, page],
  );
  const pageCount = Math.max(1, Math.ceil(currentGroups.length / pageSize));

  const resultCounts = {
    exact: result?.exactDuplicateGroups.length ?? 0,
    similar: result?.similarImageGroups.length ?? 0,
    blurry: visibleBlurryCount,
  };

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    listen<ScanProgress>("scan-progress", (event) => {
      const { message, current, total } = event.payload;
      setMessage(total > 0 ? `${message} (${current}/${total})` : message);
    }).then((cleanup) => {
      unlisten = cleanup;
    });

    return () => {
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    setPage(0);
  }, [activeTab, result]);

  async function selectFolder() {
    const selected = await open({
      directory: true,
      multiple: false,
      title: "スキャンするフォルダを選択",
    });

    if (typeof selected === "string") {
      setFolderPath(selected);
      setMessage("");
    }
  }

  async function startScan() {
    if (!folderPath) {
      setMessage("先にフォルダを選択してください。");
      return;
    }

    setIsScanning(true);
    setResult(null);
    setSelectedPaths(new Set());
    setMessage("画像をスキャンしています。");

    try {
      const scanResult = await invoke<ScanResult>("scan_images", {
        rootPath: folderPath,
        options,
      });
      setResult(scanResult);
      setMessage(
        `${scanResult.scannedCount}件を確認しました。キャッシュ利用 ${scanResult.cacheHitCount}件、スキップ ${scanResult.skippedCount}件。`,
      );
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setIsScanning(false);
    }
  }

  async function cancelScan() {
    await invoke("request_cancel_scan");
    setMessage("キャンセルを要求しました。現在の処理が区切りまで進むと止まります。");
  }

  function toggleOption(key: keyof ScanOptions) {
    setOptions((current) => ({
      ...current,
      [key]: !current[key],
    }));
  }

  function toggleSelected(path: string) {
    setSelectedPaths((current) => {
      const next = new Set(current);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }

  async function moveSelectedToTrash() {
    if (selectedPaths.size === 0) {
      setMessage("ゴミ箱へ移動する画像を選択してください。");
      return;
    }

    const confirmed = window.confirm(
      `選択した${selectedPaths.size}件の画像をゴミ箱へ移動します。よろしいですか？`,
    );
    if (!confirmed) return;

    setIsMoving(true);
    try {
      const movedPaths = await invoke<string[]>("move_to_trash", {
        paths: Array.from(selectedPaths),
      });
      removeMovedItems(movedPaths);
      setSelectedPaths(new Set());
      setMessage(`${movedPaths.length}件をゴミ箱へ移動しました。`);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setIsMoving(false);
    }
  }

  function removeMovedItems(paths: string[]) {
    const moved = new Set(paths);
    setResult((current) => {
      if (!current) return current;
      return {
        ...current,
        exactDuplicateGroups: filterGroups(current.exactDuplicateGroups, moved),
        similarImageGroups: filterGroups(current.similarImageGroups, moved),
        blurryImages: current.blurryImages.filter((item) => !moved.has(item.path)),
      };
    });
  }

  return (
    <main className="app-shell">
      <section className="result-panel">
        <div className="result-toolbar">
          <div className="tabs" role="tablist" aria-label="検出結果">
            {tabs.map((tab) => (
              <button
                key={tab.id}
                className={activeTab === tab.id ? "tab active" : "tab"}
                onClick={() => setActiveTab(tab.id)}
              >
                {tab.label}
                <span>{resultCounts[tab.id]}</span>
              </button>
            ))}
          </div>
          <div className="toolbar-actions">
            <button className="primary-button" onClick={selectFolder} disabled={isScanning}>
              <FolderOpen size={18} />
              フォルダを選択
            </button>
            <div className="folder-path">{folderPath || "スキャンするフォルダが未選択です"}</div>
            <button
              className="icon-button"
              onClick={() => setIsSettingsOpen(true)}
              disabled={isScanning}
              aria-label="設定"
              title="設定"
            >
              <Settings size={20} />
            </button>
          </div>
        </div>

        {result && currentGroups.length > pageSize && (
          <div className="pager">
            <button className="ghost-button" onClick={() => setPage((value) => Math.max(0, value - 1))} disabled={page === 0}>
              <ChevronLeft size={18} />
              前へ
            </button>
            <span>
              {page + 1} / {pageCount}
            </span>
            <button
              className="ghost-button"
              onClick={() => setPage((value) => Math.min(pageCount - 1, value + 1))}
              disabled={page >= pageCount - 1}
            >
              次へ
              <ChevronRight size={18} />
            </button>
          </div>
        )}

        {!result && <EmptyState isScanning={isScanning} />}
        {result && currentGroups.length === 0 && (
          <div className="empty-state">
            <Check size={28} />
            <p>このタブには候補がありません。</p>
          </div>
        )}
        {result && currentGroups.length > 0 && (
          <div className="group-list">
            {pagedGroups.map((group) => (
              <ImageGroupView
                key={group.id}
                group={group}
                selectedPaths={selectedPaths}
                onToggleSelected={toggleSelected}
              />
            ))}
          </div>
        )}
      </section>

      <footer className="scan-footer">
        <div className="action-row">
          <button
            className={isScanning ? "ghost-button" : "primary-button"}
            onClick={isScanning ? cancelScan : startScan}
            disabled={!isScanning && !folderPath}
          >
            {isScanning ? <Square size={18} /> : <Search size={18} />}
            {isScanning ? "キャンセル" : "スキャン開始"}
          </button>
          <button className="danger-button" onClick={moveSelectedToTrash} disabled={isMoving || selectedPaths.size === 0}>
            {isMoving ? <Loader2 className="spin" size={18} /> : <Trash2 size={18} />}
            選択した画像をゴミ箱へ
          </button>
          <button className="ghost-button" onClick={() => setSelectedPaths(new Set())} disabled={selectedPaths.size === 0}>
            <RotateCcw size={18} />
            選択を解除
          </button>
        </div>

        {message && <p className="status-message">{message}</p>}
      </footer>

      {isSettingsOpen && (
        <div className="modal-backdrop" role="presentation" onClick={() => setIsSettingsOpen(false)}>
          <section
            className="settings-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="settings-title"
            onClick={(event) => event.stopPropagation()}
          >
            <div className="modal-header">
              <h2 id="settings-title">スキャン設定</h2>
              <button className="icon-button" onClick={() => setIsSettingsOpen(false)} aria-label="設定を閉じる">
                <X size={20} />
              </button>
            </div>

            <div className="option-grid">
              <CheckOption
                label="サブフォルダを含める"
                checked={options.includeSubfolders}
                onChange={() => toggleOption("includeSubfolders")}
              />
              <CheckOption
                label="完全重複を検出"
                checked={options.detectExactDuplicates}
                onChange={() => toggleOption("detectExactDuplicates")}
              />
              <CheckOption
                label="類似画像を検出"
                checked={options.detectSimilarImages}
                onChange={() => toggleOption("detectSimilarImages")}
              />
              <CheckOption
                label="ブレ画像を検出"
                checked={options.detectBlurryImages}
                onChange={() => toggleOption("detectBlurryImages")}
              />
            </div>

            <div className="modal-footer">
              <button className="primary-button" onClick={() => setIsSettingsOpen(false)}>
                <Check size={18} />
                完了
              </button>
            </div>
          </section>
        </div>
      )}

    </main>
  );
}

function CheckOption(props: { label: string; checked: boolean; onChange: () => void }) {
  return (
    <label className="check-option">
      <input type="checkbox" checked={props.checked} onChange={props.onChange} />
      <span>{props.label}</span>
    </label>
  );
}

function EmptyState({ isScanning }: { isScanning: boolean }) {
  return (
    <div className="empty-state">
      {isScanning ? <Loader2 className="spin" size={28} /> : <ImageIcon size={28} />}
      <p>{isScanning ? "スキャン中です。" : "フォルダを選んでスキャンを開始してください。"}</p>
    </div>
  );
}

function ImageGroupView(props: {
  group: ImageGroup;
  selectedPaths: Set<string>;
  onToggleSelected: (path: string) => void;
}) {
  return (
    <article className="image-group">
      <div className="group-header">
        <h2>{props.group.title}</h2>
        <span>{props.group.items.length}件</span>
      </div>
      <div className="image-strip">
        {props.group.items.map((item) => (
          <ImageCard
            key={item.id}
            item={item}
            selected={props.selectedPaths.has(item.path)}
            onToggle={() => props.onToggleSelected(item.path)}
          />
        ))}
      </div>
    </article>
  );
}

function ImageCard(props: { item: ImageItem; selected: boolean; onToggle: () => void }) {
  return (
    <button className={props.selected ? "image-card selected" : "image-card"} onClick={props.onToggle}>
      <div className="image-preview">
        <img src={convertFileSrc(props.item.path)} alt={props.item.fileName} loading="lazy" />
        {props.selected && (
          <span className="selected-mark">
            <Check size={16} />
          </span>
        )}
      </div>
      <div className="image-meta">
        <strong title={props.item.fileName}>{props.item.fileName}</strong>
        <span>
          {props.item.width} x {props.item.height}
        </span>
        <span>{formatBytes(props.item.sizeBytes)}</span>
        {props.item.blurScore !== null && (
          <span className="warning-line">
            <AlertTriangle size={14} />
            鮮明度 {props.item.blurScore.toFixed(1)}
          </span>
        )}
      </div>
    </button>
  );
}

function filterGroups(groups: ImageGroup[], moved: Set<string>) {
  return groups
    .map((group) => ({
      ...group,
      items: group.items.filter((item) => !moved.has(item.path)),
    }))
    .filter((group) => group.items.length > 1);
}

function formatBytes(bytes: number) {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

export default App;
