import type { SnapshotEntry, TreeSnapshotFile } from "@lib/api";
import { fetchCasContent, fetchSnapshots, fetchTreeAt } from "@lib/api";
import { events } from "@stores/events.store";
import { createMemo, createSignal } from "solid-js";

// --- Types ---

export interface Version {
  seq: number;
  ts_wall: string;
  afterHash: string | null;
  data: string | null;
  size: number;
  /** True when this version arrived via a .tmp → rename chain (agent-authored). */
  fromRename: boolean;
}

export interface RenameEntry {
  seq: number;
  oldPath: string;
  newPath: string;
}

export interface FileEntry {
  path: string;
  versions: Version[];
  renames: RenameEntry[];
  deleted: boolean;
}

export interface TreeNode {
  name: string;
  path: string;
  isDir: boolean;
  deleted: boolean;
  children: TreeNode[];
  fileEntry: FileEntry | null;
}

// --- State ---

const [selectedPath, setSelectedPath] = createSignal<string | null>(null);
const [selectedVersionIdx, setSelectedVersionIdx] = createSignal<number | null>(null);
const [compareVersionIdx, setCompareVersionIdx] = createSignal<number | null>(null);
const [workspaceRoot, setWorkspaceRoot] = createSignal<string | null>(null);

// --- Snapshot browsing state ---

/** 'live' shows real-time event-derived tree; a number selects a snapshot seq. */
const [snapshotMode, setSnapshotMode] = createSignal<"live" | number>("live");
const [snapshots, setSnapshots] = createSignal<SnapshotEntry[]>([]);
const [snapshotTreeFiles, setSnapshotTreeFiles] = createSignal<TreeSnapshotFile[] | null>(null);
const [snapshotFileContent, setSnapshotFileContent] = createSignal<string | null>(null);
const [snapshotFileHash, setSnapshotFileHash] = createSignal<string | null>(null);

let snapshotPollInterval: ReturnType<typeof setInterval> | null = null;

// --- Write coalescing ---

/** Threshold in ms — writes to the same path within this window are one version. */
const COALESCE_MS = 1000;

function parseTs(ts: string): number {
  return new Date(ts).getTime();
}

/**
 * Coalesce rapid-fire writes to the same path into a single version.
 * npm/yarn download chunks appear as many writes within milliseconds —
 * we keep only the last write in each burst.
 */
function coalesceWrites(versions: Version[]): Version[] {
  if (versions.length <= 1) return versions;

  const result: Version[] = [];
  let burstEnd = versions[0] as Version;

  for (let i = 1; i < versions.length; i++) {
    const cur = versions[i] as Version;
    const gap = parseTs(cur.ts_wall) - parseTs(burstEnd.ts_wall);
    if (gap <= COALESCE_MS) {
      // Same burst — advance to this write (keep the latest)
      burstEnd = cur;
    } else {
      // New burst — flush previous
      result.push(burstEnd);
      burstEnd = cur;
    }
  }
  result.push(burstEnd);
  return result;
}

// --- Derived state ---

/** Build a map of path → FileEntry from the event stream. */
// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: event processing handles many event types
export const fileMap = createMemo<Map<string, FileEntry>>(() => {
  const root = workspaceRoot();
  const map = new Map<string, FileEntry>();

  // Auto-detect workspace from agent_start event
  for (const event of events) {
    if (event.type === "agent_start") {
      const summary = event.config_summary;
      if (summary && !root) {
        const match = summary.match(/workspace=([^\s,]+)/);
        if (match?.[1]) setWorkspaceRoot(match[1]);
      }
      break;
    }
  }

  const effectiveRoot = workspaceRoot();

  for (const event of events) {
    const type = event.type;

    if (type === "write") {
      const path = event.path as string | undefined;
      if (!path) continue;
      if (effectiveRoot && !path.startsWith(effectiveRoot)) continue;

      const version: Version = {
        seq: event.seq,
        ts_wall: event.ts_wall,
        afterHash: event.after_hash ?? null,
        data: event.data ?? null,
        size: Number(event.size ?? 0),
        fromRename: false,
      };

      let entry = map.get(path);
      if (!entry) {
        entry = { path, versions: [], renames: [], deleted: false };
        map.set(path, entry);
      }
      entry.versions.push(version);
      entry.deleted = false;
    } else if (type === "rename") {
      const oldPath = event.old_path;
      const newPath = event.new_path;
      if (!oldPath || !newPath) continue;
      if (effectiveRoot && !newPath.startsWith(effectiveRoot) && !oldPath.startsWith(effectiveRoot))
        continue;

      let targetEntry = map.get(newPath);
      if (!targetEntry) {
        targetEntry = { path: newPath, versions: [], renames: [], deleted: false };
        map.set(newPath, targetEntry);
      }

      targetEntry.renames.push({ seq: event.seq, oldPath, newPath });
      targetEntry.deleted = false;

      // Move versions from old path to new path (temp → final), tag as rename-sourced
      const oldEntry = map.get(oldPath);
      if (oldEntry) {
        for (const v of oldEntry.versions) {
          v.fromRename = true;
          targetEntry.versions.push(v);
        }
        map.delete(oldPath);
      }
    } else if (type === "unlink") {
      const path = event.path as string | undefined;
      if (!path) continue;
      if (effectiveRoot && !path.startsWith(effectiveRoot)) continue;

      const entry = map.get(path);
      if (entry) {
        entry.deleted = true;
      }
    } else if (type === "mkdir") {
      const path = event.path as string | undefined;
      if (!path) continue;
      if (effectiveRoot && !path.startsWith(effectiveRoot)) continue;

      if (!map.has(path)) {
        map.set(path, { path, versions: [], renames: [], deleted: false });
      }
    }
  }

  for (const entry of map.values()) {
    if (entry.versions.length > 1) {
      entry.versions.sort((a, b) => a.seq - b.seq);
      entry.versions = coalesceWrites(entry.versions);
    }

    // Agent writes always go through .tmp+rename. If a file has any
    // rename-sourced versions, direct writes are tool noise (npm, etc.).
    const hasRenameVersions = entry.versions.some((v) => v.fromRename);
    if (hasRenameVersions) {
      entry.versions = entry.versions.filter((v) => v.fromRename);
    }
  }

  return map;
});

/** Build a tree structure from the file map for sidebar rendering. */
// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: tree building is inherently recursive
export const fileTree = createMemo<TreeNode>(() => {
  const root = workspaceRoot() ?? "/";
  const map = fileMap();

  const rootNode: TreeNode = {
    name: root.split("/").pop() || root,
    path: root,
    isDir: true,
    deleted: false,
    children: [],
    fileEntry: null,
  };

  for (const entry of map.values()) {
    const relPath = entry.path.startsWith(`${root}/`)
      ? entry.path.slice(root.length + 1)
      : entry.path.slice(root.length);
    if (!relPath) continue;

    const parts = relPath.split("/");
    let current = rootNode;

    for (let i = 0; i < parts.length; i++) {
      const part = parts[i] as string;
      const isLast = i === parts.length - 1;

      let child = current.children.find((c) => c.name === part);
      if (!child) {
        const childPath = `${root}/${parts.slice(0, i + 1).join("/")}`;
        child = {
          name: part,
          path: childPath,
          isDir: !isLast || entry.versions.length === 0,
          deleted: false,
          children: [],
          fileEntry: null,
        };
        current.children.push(child);
      }

      if (isLast) {
        child.fileEntry = entry;
        child.deleted = entry.deleted;
        child.isDir = entry.versions.length === 0 && !entry.deleted;
      }

      current = child;
    }
  }

  function sortTree(node: TreeNode) {
    node.children.sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
    for (const child of node.children) {
      sortTree(child);
    }
  }
  sortTree(rootNode);

  return rootNode;
});

/** The currently selected file entry. */
export const selectedFile = createMemo<FileEntry | null>(() => {
  const path = selectedPath();
  if (!path) return null;
  return fileMap().get(path) ?? null;
});

// --- Actions ---

export function selectFile(path: string) {
  setSelectedPath(path);
  const entry = fileMap().get(path);
  if (entry && entry.versions.length > 0) {
    setSelectedVersionIdx(entry.versions.length - 1);
  } else {
    setSelectedVersionIdx(null);
  }
  setCompareVersionIdx(null);
}

export function selectVersion(idx: number) {
  setSelectedVersionIdx(idx);
  // Auto-diff: when selecting a non-latest version, compare against previous
  const file = selectedFile();
  if (file && idx > 0) {
    setCompareVersionIdx(idx - 1);
  } else {
    setCompareVersionIdx(null);
  }
}

export function toggleCompareVersion(idx: number) {
  if (compareVersionIdx() === idx) {
    setCompareVersionIdx(null);
  } else {
    setCompareVersionIdx(idx);
  }
}

export function clearFileSelection() {
  setSelectedPath(null);
  setSelectedVersionIdx(null);
  setCompareVersionIdx(null);
}

export {
  compareVersionIdx,
  selectedPath,
  selectedVersionIdx,
  setWorkspaceRoot,
  snapshotFileContent,
  snapshotFileHash,
  snapshotMode,
  snapshots,
  workspaceRoot,
};

// --- Snapshot browsing ---

/** Build a tree structure from snapshot API files for sidebar rendering. */
// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: tree building is inherently recursive
export const snapshotFileTree = createMemo<TreeNode | null>(() => {
  const files = snapshotTreeFiles();
  if (!files) return null;

  const root = workspaceRoot() ?? "/";
  const rootNode: TreeNode = {
    name: root.split("/").pop() || root,
    path: root,
    isDir: true,
    deleted: false,
    children: [],
    fileEntry: null,
  };

  for (const file of files) {
    const relPath = file.path.startsWith(`${root}/`)
      ? file.path.slice(root.length + 1)
      : file.path.startsWith(root)
        ? file.path.slice(root.length)
        : file.path;
    if (!relPath) continue;

    const parts = relPath.split("/");
    let current = rootNode;

    for (let i = 0; i < parts.length; i++) {
      const part = parts[i] as string;
      const isLast = i === parts.length - 1;

      let child = current.children.find((c) => c.name === part);
      if (!child) {
        const childPath = `${root}/${parts.slice(0, i + 1).join("/")}`;
        child = {
          name: part,
          path: childPath,
          isDir: !isLast,
          deleted: false,
          children: [],
          fileEntry: null,
        };
        current.children.push(child);
      }

      if (isLast) {
        child.fileEntry = {
          path: file.path,
          versions: [],
          renames: [],
          deleted: false,
        };
        child.isDir = false;
      }

      current = child;
    }
  }

  function sortTree(node: TreeNode) {
    node.children.sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
    for (const child of node.children) {
      sortTree(child);
    }
  }
  sortTree(rootNode);

  return rootNode;
});

/** Snapshot file hash lookup by path. */
const snapshotHashMap = createMemo<Map<string, string>>(() => {
  const files = snapshotTreeFiles();
  if (!files) return new Map();
  const map = new Map<string, string>();
  for (const f of files) {
    map.set(f.path, f.hash);
  }
  return map;
});

/** Start polling for snapshot list updates. */
export async function startSnapshotPolling() {
  const poll = async () => {
    try {
      const snaps = await fetchSnapshots();
      setSnapshots(snaps);
    } catch {
      // ignore fetch errors — dashboard may load before supervisor
    }
  };
  await poll();
  snapshotPollInterval = setInterval(poll, 5000);
}

/** Stop snapshot polling. */
export function stopSnapshotPolling() {
  if (snapshotPollInterval) {
    clearInterval(snapshotPollInterval);
    snapshotPollInterval = null;
  }
}

/** Switch to a snapshot or back to live mode. */
export async function selectSnapshot(mode: "live" | number) {
  if (mode === "live") {
    setSnapshotMode("live");
    setSnapshotTreeFiles(null);
    setSnapshotFileContent(null);
    setSnapshotFileHash(null);
    clearFileSelection();
    return;
  }
  setSnapshotMode(mode);
  setSnapshotFileContent(null);
  setSnapshotFileHash(null);
  clearFileSelection();
  try {
    const tree = await fetchTreeAt(mode);
    setSnapshotTreeFiles(tree.files);
  } catch {
    setSnapshotTreeFiles(null);
  }
}

/** Select a file in snapshot browsing mode and fetch its content from CAS. */
export async function selectSnapshotFile(path: string) {
  setSelectedPath(path);
  setSelectedVersionIdx(null);
  setCompareVersionIdx(null);
  const hash = snapshotHashMap().get(path);
  if (!hash) {
    setSnapshotFileContent(null);
    setSnapshotFileHash(null);
    return;
  }
  setSnapshotFileHash(hash);
  try {
    const content = await fetchCasContent(hash);
    setSnapshotFileContent(content);
  } catch {
    setSnapshotFileContent(null);
  }
}

// --- Diff utility ---

export interface DiffLine {
  type: "same" | "add" | "remove";
  line: string;
  oldLineNo: number | null;
  newLineNo: number | null;
}

/** Max lines for LCS diff — beyond this the O(n*m) table is too expensive. */
const DIFF_LINE_LIMIT = 5000;

/** LCS-based unified diff. O(n*m) — guarded to avoid freezing on large files. */
// biome-ignore lint/complexity/noExcessiveCognitiveComplexity: diff algorithm is inherently complex
export function computeDiff(oldText: string, newText: string): DiffLine[] {
  const oldLines = oldText.split("\n");
  const newLines = newText.split("\n");
  const m = oldLines.length;
  const n = newLines.length;

  if (m > DIFF_LINE_LIMIT || n > DIFF_LINE_LIMIT) {
    return [
      {
        type: "remove",
        line: `[file too large for inline diff (${m} / ${n} lines)]`,
        oldLineNo: null,
        newLineNo: null,
      },
    ];
  }

  // Build LCS table
  const dp: number[][] = Array.from({ length: m + 1 }, () => new Array<number>(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (oldLines[i - 1] === newLines[j - 1]) {
        (dp[i] as number[])[j] = (dp[i - 1]?.[j - 1] ?? 0) + 1;
      } else {
        (dp[i] as number[])[j] = Math.max(dp[i - 1]?.[j] ?? 0, dp[i]?.[j - 1] ?? 0);
      }
    }
  }

  const result: DiffLine[] = [];
  let i = m;
  let j = n;

  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldLines[i - 1] === newLines[j - 1]) {
      result.push({ type: "same", line: oldLines[i - 1] ?? "", oldLineNo: i, newLineNo: j });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || (dp[i]?.[j - 1] ?? 0) >= (dp[i - 1]?.[j] ?? 0))) {
      result.push({ type: "add", line: newLines[j - 1] ?? "", oldLineNo: null, newLineNo: j });
      j--;
    } else {
      result.push({ type: "remove", line: oldLines[i - 1] ?? "", oldLineNo: i, newLineNo: null });
      i--;
    }
  }

  result.reverse();
  return result;
}
