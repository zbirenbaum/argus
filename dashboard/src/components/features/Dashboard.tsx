import { cleanupStream, connected, events, initialize } from "@stores/events.store";
import { cn } from "@utils/cn";
import { createSignal, onCleanup, onMount } from "solid-js";
import { Show } from "solid-js/web";
import { EventViewer } from "./EventViewer";
import { FilesViewer } from "./FilesViewer";
import { NetworkViewer } from "./NetworkViewer";

type Tab = "events" | "network" | "files";

export function Dashboard() {
  const [activeTab, setActiveTab] = createSignal<Tab>("events");

  onMount(() => {
    void initialize();
  });

  onCleanup(() => {
    cleanupStream();
  });

  return (
    <div class="flex h-screen w-full flex-col">
      {/* Tab bar */}
      <nav class="flex shrink-0 items-center gap-1 border-b border-[hsl(var(--border))] bg-[hsl(var(--background))] px-3">
        <TabButton label="Events" tab="events" active={activeTab()} onClick={setActiveTab} />
        <TabButton label="Files" tab="files" active={activeTab()} onClick={setActiveTab} />
        <TabButton label="Network" tab="network" active={activeTab()} onClick={setActiveTab} />
        <div class="ml-auto flex items-center gap-3 py-2">
          <div
            class={cn("h-2 w-2 shrink-0 rounded-full", connected() ? "bg-green-500" : "bg-red-500")}
          />
          <span class="shrink-0 text-xs font-mono text-[hsl(var(--muted-foreground))]">
            {events.length} events
          </span>
        </div>
      </nav>

      {/* Tab content */}
      <div class="min-h-0 flex-1">
        <Show when={activeTab() === "events"}>
          <EventViewer />
        </Show>
        <Show when={activeTab() === "files"}>
          <FilesViewer />
        </Show>
        <Show when={activeTab() === "network"}>
          <NetworkViewer />
        </Show>
      </div>
    </div>
  );
}

function TabButton(props: { label: string; tab: Tab; active: Tab; onClick: (tab: Tab) => void }) {
  const isActive = () => props.active === props.tab;

  return (
    <button
      type="button"
      class={cn(
        "px-3 py-2 text-sm font-medium transition-colors",
        "border-b-2 -mb-px",
        isActive()
          ? "border-[hsl(var(--foreground))] text-[hsl(var(--foreground))]"
          : "border-transparent text-[hsl(var(--muted-foreground))] hover:text-[hsl(var(--foreground))]",
      )}
      onClick={() => props.onClick(props.tab)}
    >
      {props.label}
    </button>
  );
}
