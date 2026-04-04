import { restoreFile } from "@lib/api";
import { formatTime } from "@lib/format";
import type { DiffLine, TreeNode, Version } from "@stores/files.store";
import {
  compareVersionIdx,
  computeDiff,
  fileTree,
  selectedFile,
  selectedPath,
  selectedVersionIdx,
  selectFile,
  selectSnapshot,
  selectSnapshotFile,
  selectVersion,
  snapshotFileContent,
  snapshotFileHash,
  snapshotFileTree,
  snapshotMode,
  snapshots,
  startSnapshotPolling,
  toggleCompareVersion,
} from "@stores/files.store";
import { cn } from "@utils/cn";
import { createMemo, createSignal, onMount } from "solid-js";
import { For, Show } from "solid-js/web";

// --- Main component ---

export function FilesViewer() {
  onMount(() => {
    startSnapshotPolling();
  });

  return (
    <div class="flex h-full w-full">
      <FileTreeSidebar />
      <div class="flex min-w-0 flex-1 flex-col">
        <VersionBar />
        <ContentPanel />
      </div>
    </div>
  );
}

// --- Snapshot selector ---

function SnapshotSelector() {
  const mode = snapshotMode;
  const snaps = snapshots;

  return (
    <div class="border-b border-[hsl(var(--border))] px-2 py-1.5">
      <select
        class={cn(
          "w-full rounded-[var(--radius-sm)] border px-2 py-1 text-xs",
          "border-[hsl(var(--border))] bg-[hsl(var(--background))] text-[hsl(var(--foreground))]",
          mode() !== "live" && "ring-1 ring-amber-500/50 border-amber-500/50",
        )}
        value={mode() === "live" ? "live" : String(mode())}
        onChange={(e) => {
          const val = e.currentTarget.value;
          if (val === "live") {
            selectSnapshot("live");
          } else {
            selectSnapshot(Number(val));
          }
        }}
      >
        <option value="live">Live</option>
        <For each={snaps()}>
          {(snap) => (
            <option value={String(snap.seq)}>
              {formatTime(snap.ts_wall)} ({snap.file_count} files)
            </option>
          )}
        </For>
      </select>
    </div>
  );
}

// --- File tree sidebar ---

function FileTreeSidebar() {
  const mode = snapshotMode;
  const liveTree = fileTree;
  const snapTree = snapshotFileTree;

  const activeTree = createMemo(() => {
    if (mode() !== "live") {
      return snapTree();
    }
    return liveTree();
  });

  return (
    <aside
      class={cn(
        "w-[280px] shrink-0 overflow-y-auto border-r",
        "border-[hsl(var(--border))] bg-[hsl(var(--background))]",
      )}
    >
      <div class="p-2">
        <h2 class="px-2 py-1.5 text-xs font-semibold uppercase tracking-wider text-[hsl(var(--muted-foreground))]">
          Files
        </h2>
      </div>
      <SnapshotSelector />
      <Show when={mode() !== "live"}>
        <div class="mx-2 my-1 rounded-[var(--radius-sm)] bg-amber-500/10 px-2 py-1 text-[10px] text-amber-400">
          Browsing snapshot
        </div>
      </Show>
      <Show
        when={activeTree() && (activeTree()?.children.length ?? 0) > 0}
        fallback={
          <p class="px-4 py-4 text-sm text-[hsl(var(--muted-foreground))]">
            {mode() === "live" ? "No file events yet" : "No files in snapshot"}
          </p>
        }
      >
        <div class="px-1 pb-2">
          <For each={activeTree()?.children}>
            {(node) => <TreeNodeRow node={node} depth={0} defaultOpen={false} />}
          </For>
        </div>
      </Show>
    </aside>
  );
}

function TreeNodeRow(props: { node: TreeNode; depth: number; defaultOpen: boolean }) {
  const [expanded, setExpanded] = createSignal(props.defaultOpen);
  const isSelected = () => selectedPath() === props.node.path;
  const hasChildren = () => props.node.children.length > 0;
  const isDir = () => props.node.isDir || hasChildren();
  const paddingLeft = () => `${props.depth * 16 + 8}px`;
  const mode = snapshotMode;

  function handleClick() {
    if (isDir()) {
      setExpanded((v) => !v);
    }
    if (props.node.fileEntry) {
      if (mode() !== "live") {
        selectSnapshotFile(props.node.path);
      } else {
        selectFile(props.node.path);
      }
    }
  }

  return (
    <>
      <button
        type="button"
        class={cn(
          "flex w-full items-center gap-1.5 rounded-[var(--radius-sm)] py-1 pr-2 text-left text-sm",
          "hover:bg-[hsl(var(--muted))] transition-colors",
          isSelected() && "bg-[hsl(var(--accent))]",
          props.node.deleted && "opacity-50",
        )}
        style={{ "padding-left": paddingLeft() }}
        onClick={handleClick}
      >
        <Show when={isDir()} fallback={<span class="w-3.5 shrink-0" />}>
          <svg
            aria-hidden="true"
            class={cn("h-3.5 w-3.5 shrink-0 transition-transform", expanded() && "rotate-90")}
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M9 18l6-6-6-6" />
          </svg>
        </Show>

        <span
          class={cn(
            "shrink-0 text-xs font-mono w-4 text-center",
            isDir() ? "text-[hsl(var(--muted-foreground))]" : "text-[hsl(var(--foreground))]",
          )}
        >
          {props.node.deleted ? "x" : isDir() ? "d" : "f"}
        </span>

        <span class="truncate">{props.node.name}</span>

        <Show
          when={
            mode() === "live" && props.node.fileEntry && props.node.fileEntry.versions.length > 0
          }
        >
          <span
            class={cn(
              "ml-auto shrink-0 rounded-full px-1.5 py-0.5 text-[10px] leading-none",
              "bg-[hsl(var(--secondary))] text-[hsl(var(--secondary-foreground))]",
            )}
          >
            {props.node.fileEntry?.versions.length}
          </span>
        </Show>
      </button>

      <Show when={isDir() && expanded()}>
        <For each={props.node.children}>
          {(child) => <TreeNodeRow node={child} depth={props.depth + 1} defaultOpen={false} />}
        </For>
      </Show>
    </>
  );
}

// --- Version bar ---

function RestoreButton() {
  const [status, setStatus] = createSignal<"idle" | "loading" | "success" | "error">("idle");
  const [errorMsg, setErrorMsg] = createSignal("");

  const file = selectedFile;
  const vIdx = selectedVersionIdx;

  async function handleRestore() {
    const f = file();
    const idx = vIdx();
    if (!f || idx === null) return;
    const version = f.versions[idx];
    if (!version?.afterHash) return;

    setStatus("loading");
    try {
      await restoreFile(f.path, version.afterHash);
      setStatus("success");
      setTimeout(() => setStatus("idle"), 2000);
    } catch (e) {
      setErrorMsg(e instanceof Error ? e.message : "Unknown error");
      setStatus("error");
      setTimeout(() => setStatus("idle"), 3000);
    }
  }

  return (
    <Show when={file() && vIdx() !== null}>
      <button
        type="button"
        class={cn(
          "ml-auto shrink-0 rounded-[var(--radius-sm)] px-2.5 py-0.5 text-xs font-medium transition-colors",
          status() === "idle" &&
            "bg-[hsl(var(--primary))] text-[hsl(var(--primary-foreground))] hover:opacity-80",
          status() === "loading" &&
            "bg-[hsl(var(--muted))] text-[hsl(var(--muted-foreground))] cursor-wait",
          status() === "success" && "bg-green-600 text-white",
          status() === "error" && "bg-red-600 text-white",
        )}
        disabled={status() !== "idle"}
        onClick={handleRestore}
        title={status() === "error" ? errorMsg() : "Restore file to this version"}
      >
        {status() === "idle" && "Restore"}
        {status() === "loading" && "Restoring..."}
        {status() === "success" && "Restored"}
        {status() === "error" && "Failed"}
      </button>
    </Show>
  );
}

function VersionBar() {
  const file = selectedFile;
  const mode = snapshotMode;

  return (
    <div
      class={cn(
        "flex shrink-0 items-center gap-1 overflow-x-auto border-b",
        "border-[hsl(var(--border))] bg-[hsl(var(--background))] px-3 py-1.5",
      )}
    >
      <Show when={mode() !== "live"}>
        <span class="mr-2 shrink-0 font-mono text-xs text-amber-400">
          {selectedPath()?.replace(/^\/workspace\/|^\/tmp\/workspace\//, "") ?? "Select a file"}
        </span>
        <Show when={snapshotFileHash()}>
          <span class="shrink-0 text-[10px] text-[hsl(var(--muted-foreground))] font-mono">
            {snapshotFileHash()?.slice(0, 12)}...
          </span>
        </Show>
      </Show>
      <Show when={mode() === "live"}>
        <Show
          when={file()}
          fallback={<span class="text-xs text-[hsl(var(--muted-foreground))]">Select a file</span>}
        >
          {(f) => (
            <>
              <span class="mr-2 shrink-0 font-mono text-xs text-[hsl(var(--muted-foreground))]">
                {f().path.replace(/^\/workspace\/|^\/tmp\/workspace\//, "")}
              </span>
              <Show when={f().versions.length > 0}>
                <For each={f().versions}>
                  {(version, idx) => {
                    const isActive = () => selectedVersionIdx() === idx();
                    const isCompare = () => compareVersionIdx() === idx();

                    return (
                      <button
                        type="button"
                        class={cn(
                          "shrink-0 rounded-[var(--radius-sm)] px-2 py-0.5 text-xs font-mono transition-colors",
                          isActive()
                            ? "bg-[hsl(var(--foreground))] text-[hsl(var(--background))]"
                            : isCompare()
                              ? "bg-blue-500/20 text-blue-400 ring-1 ring-blue-500/40"
                              : "hover:bg-[hsl(var(--muted))] text-[hsl(var(--muted-foreground))]",
                        )}
                        onClick={(e) => {
                          if (e.shiftKey || e.metaKey) {
                            toggleCompareVersion(idx());
                          } else {
                            selectVersion(idx());
                          }
                        }}
                        title={`v${idx() + 1} — ${formatTime(version.ts_wall)}\nClick to view, Shift+click to set as diff base`}
                      >
                        v{idx() + 1}
                        <span class="ml-1 text-[10px] opacity-70">
                          {formatTime(version.ts_wall)}
                        </span>
                      </button>
                    );
                  }}
                </For>
              </Show>
              <Show when={f().renames.length > 0}>
                <For each={f().renames}>
                  {(rename) => (
                    <span
                      class="shrink-0 rounded-full bg-blue-500/15 px-2 py-0.5 text-[10px] leading-none text-blue-400"
                      title={`Renamed from ${rename.oldPath}`}
                    >
                      {rename.oldPath.split("/").pop()}
                    </span>
                  )}
                </For>
              </Show>
              <Show when={f().deleted}>
                <span class="shrink-0 rounded-full bg-red-500/15 px-2 py-0.5 text-[10px] leading-none text-red-400">
                  deleted
                </span>
              </Show>

              {/* Diff mode indicator */}
              <Show when={compareVersionIdx() !== null}>
                <span class="shrink-0 text-[10px] text-blue-400">
                  diff: v{(compareVersionIdx() ?? 0) + 1} → v{(selectedVersionIdx() ?? 0) + 1}
                </span>
              </Show>

              <RestoreButton />
            </>
          )}
        </Show>
      </Show>
    </div>
  );
}

// --- Content panel ---

function ContentPanel() {
  const file = selectedFile;
  const versionIdx = selectedVersionIdx;
  const cmpIdx = compareVersionIdx;
  const mode = snapshotMode;

  const currentVersion = createMemo<Version | null>(() => {
    const f = file();
    const idx = versionIdx();
    if (!f || idx === null || idx >= f.versions.length) return null;
    return f.versions[idx] ?? null;
  });

  const compareVersion = createMemo<Version | null>(() => {
    const f = file();
    const idx = cmpIdx();
    if (!f || idx === null || idx >= f.versions.length) return null;
    return f.versions[idx] ?? null;
  });

  const diff = createMemo<DiffLine[] | null>(() => {
    const cur = currentVersion();
    const cmp = compareVersion();
    if (!cur || !cmp) return null;
    return computeDiff(cmp.data ?? "", cur.data ?? "");
  });

  return (
    <main class="flex-1 overflow-auto bg-[hsl(var(--background))]">
      {/* Snapshot mode content */}
      <Show when={mode() !== "live"}>
        <Show
          when={selectedPath()}
          fallback={
            <div class="flex h-full items-center justify-center">
              <p class="text-[hsl(var(--muted-foreground))]">Select a file to view its contents</p>
            </div>
          }
        >
          <Show
            when={snapshotFileContent() !== null}
            fallback={
              <div class="flex h-full items-center justify-center">
                <p class="text-[hsl(var(--muted-foreground))]">Loading content...</p>
              </div>
            }
          >
            <SnapshotContentView content={snapshotFileContent() ?? ""} />
          </Show>
        </Show>
      </Show>

      {/* Live mode content */}
      <Show when={mode() === "live"}>
        <Show
          when={file()}
          fallback={
            <div class="flex h-full items-center justify-center">
              <p class="text-[hsl(var(--muted-foreground))]">Select a file to view its contents</p>
            </div>
          }
        >
          <Show
            when={currentVersion()}
            fallback={
              <div class="flex h-full items-center justify-center">
                <p class="text-[hsl(var(--muted-foreground))]">No content available</p>
              </div>
            }
          >
            {(version) => (
              <Show when={diff()} fallback={<ContentView version={version()} />}>
                {(d) => <DiffView lines={d()} />}
              </Show>
            )}
          </Show>
        </Show>
      </Show>
    </main>
  );
}

function SnapshotContentView(props: { content: string }) {
  const lines = createMemo(() => props.content.split("\n"));

  return (
    <div class="font-mono text-sm">
      <table class="w-full border-collapse">
        <tbody>
          <For each={lines()}>
            {(line, idx) => (
              <tr class="hover:bg-[hsl(var(--muted))]">
                <td class="w-12 select-none border-r border-[hsl(var(--border))] px-2 py-px text-right text-xs text-[hsl(var(--muted-foreground))]">
                  {idx() + 1}
                </td>
                <td class="whitespace-pre-wrap break-all px-3 py-px">{line}</td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}

function ContentView(props: { version: Version }) {
  const lines = createMemo(() => (props.version.data ?? "").split("\n"));

  return (
    <div class="font-mono text-sm">
      <Show
        when={props.version.data !== null}
        fallback={
          <div class="p-6 text-[hsl(var(--muted-foreground))]">
            <p>Content not available inline</p>
            <p class="mt-1 text-xs">Hash: {props.version.afterHash ?? "unknown"}</p>
            <p class="text-xs">Size: {props.version.size} bytes</p>
          </div>
        }
      >
        <table class="w-full border-collapse">
          <tbody>
            <For each={lines()}>
              {(line, idx) => (
                <tr class="hover:bg-[hsl(var(--muted))]">
                  <td class="w-12 select-none border-r border-[hsl(var(--border))] px-2 py-px text-right text-xs text-[hsl(var(--muted-foreground))]">
                    {idx() + 1}
                  </td>
                  <td class="whitespace-pre-wrap break-all px-3 py-px">{line}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </div>
  );
}

function DiffView(props: { lines: DiffLine[] }) {
  return (
    <div class="font-mono text-sm">
      <table class="w-full border-collapse">
        <tbody>
          <For each={props.lines}>
            {/* biome-ignore lint/complexity/noExcessiveCognitiveComplexity: diff row rendering uses ternaries for 3 states */}
            {(dl) => (
              <tr
                class={cn(
                  dl.type === "add" && "bg-green-900/30",
                  dl.type === "remove" && "bg-red-900/30",
                )}
              >
                <td
                  class={cn(
                    "w-10 select-none border-r border-[hsl(var(--border))] px-1 py-px text-right text-xs",
                    dl.type === "remove"
                      ? "text-red-400/70"
                      : "text-[hsl(var(--muted-foreground))]",
                  )}
                >
                  {dl.oldLineNo ?? ""}
                </td>
                <td
                  class={cn(
                    "w-10 select-none border-r border-[hsl(var(--border))] px-1 py-px text-right text-xs",
                    dl.type === "add" ? "text-green-400/70" : "text-[hsl(var(--muted-foreground))]",
                  )}
                >
                  {dl.newLineNo ?? ""}
                </td>
                <td
                  class={cn(
                    "w-6 select-none px-1 py-px text-center text-xs font-bold",
                    dl.type === "add" && "text-green-400",
                    dl.type === "remove" && "text-red-400",
                  )}
                >
                  {dl.type === "add" ? "+" : dl.type === "remove" ? "\u2212" : ""}
                </td>
                <td
                  class={cn(
                    "whitespace-pre-wrap break-all px-2 py-px",
                    dl.type === "add" && "text-green-300",
                    dl.type === "remove" && "text-red-300 line-through decoration-red-500/40",
                  )}
                >
                  {dl.line}
                </td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </div>
  );
}
